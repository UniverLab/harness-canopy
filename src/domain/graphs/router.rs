use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::GraphEdge;

/// Minimum/maximum number of routes a [`super::GraphNodeKind::Router`] node may
/// declare.
pub const ROUTER_MIN_ROUTES: usize = 2;
pub const ROUTER_MAX_ROUTES: usize = 8;

/// The values a router's optional `mode` config field may take. Absent means
/// `"model"` — the historical agent/decision router.
pub const ROUTER_MODES: &[&str] = &["model", "value"];

/// Every source a deterministic (`mode: "value"`) router can read. One list,
/// shared by config validation and by the engine's dispatch.
pub const VALUE_ROUTER_SOURCES: &[&str] =
    &["spec_type", "spec_scope", "spec_tier", "previous_output"];

/// Minimum number of routes a value router may declare. The model router's
/// two-route minimum does not apply: one known value can be enough.
pub const VALUE_ROUTER_MIN_ROUTES: usize = 1;

/// One route a router node can select: a short label plus a one-line
/// description of when to take it. Declared in the node's `config` (the
/// `routes` array) and referenced by an edge's [`super::GraphEdgeCondition::Route`]
/// label — never persisted separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterRoute {
    pub label: String,
    pub description: String,
}

/// Validate a router node's declared routes + fallback shape: `2`-`8`
/// routes, unique non-empty labels each with a non-empty description, and a
/// `fallback` that names one of them. Pure config-shape check — it doesn't
/// need the graph's edges, so it applies equally at node add and update
/// time (see `daemon::handler::validate_node_config`'s `Router` arm).
pub fn validate_router_routes(routes: &[RouterRoute], fallback: &str) -> Result<(), String> {
    if routes.len() < ROUTER_MIN_ROUTES {
        return Err(format!(
            "A router must declare at least {ROUTER_MIN_ROUTES} routes, got {}.",
            routes.len()
        ));
    }
    if routes.len() > ROUTER_MAX_ROUTES {
        return Err(format!(
            "A router must declare at most {ROUTER_MAX_ROUTES} routes, got {}.",
            routes.len()
        ));
    }

    let mut seen_labels = std::collections::HashSet::new();
    for route in routes {
        if route.label.trim().is_empty() {
            return Err("Every router route must have a non-empty label.".to_string());
        }
        if route.description.trim().is_empty() {
            return Err(format!(
                "Router route '{}' must have a non-empty description.",
                route.label
            ));
        }
        if !seen_labels.insert(route.label.as_str()) {
            return Err(format!(
                "Router route label '{}' is declared more than once.",
                route.label
            ));
        }
    }

    if fallback.trim().is_empty() {
        return Err("A router must declare one route as fallback.".to_string());
    }
    if !routes.iter().any(|route| route.label == fallback) {
        return Err(format!(
            "Router fallback '{fallback}' does not name a declared route."
        ));
    }

    Ok(())
}

/// Validate that every `route`-conditioned edge out of `node_id` names a
/// route actually declared by `routes` — called whenever a router's routes
/// list or its edges change, so an edge can never be left pointing at a
/// route that no longer exists.
pub fn validate_router_edges_declared(
    routes: &[RouterRoute],
    node_id: &str,
    edges: &[GraphEdge],
) -> Result<(), String> {
    for edge in edges.iter().filter(|edge| edge.from_node == node_id) {
        if let Some(label) = edge.condition.route_label() {
            if !routes.iter().any(|route| route.label == label) {
                return Err(format!(
                    "Edge '{}' names undeclared route '{label}'.",
                    edge.id
                ));
            }
        }
    }
    Ok(())
}

/// Validate that every declared route already has at least one outgoing
/// `route` edge from `node_id` — but only once the router has begun being
/// wired (at least one `route` edge already exists from it). A brand new
/// router with no edges at all is a valid, not-yet-wired state; once wiring
/// starts, every declared route must be covered.
pub fn validate_router_route_coverage(
    routes: &[RouterRoute],
    node_id: &str,
    edges: &[GraphEdge],
) -> Result<(), String> {
    let route_edges: Vec<&GraphEdge> = edges
        .iter()
        .filter(|edge| edge.from_node == node_id && edge.condition.route_label().is_some())
        .collect();
    if route_edges.is_empty() {
        return Ok(());
    }
    for route in routes {
        let served = route_edges
            .iter()
            .any(|edge| edge.condition.route_label() == Some(route.label.as_str()));
        if !served {
            return Err(format!(
                "Router route '{}' has no outgoing edge.",
                route.label
            ));
        }
    }
    Ok(())
}

/// A parsed `mode: "value"` router config: the source it reads, the routes a
/// value can select, and the optional default route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueRouterConfig {
    pub source: String,
    pub routes: Vec<RouterRoute>,
    pub default_route: Option<String>,
}

/// The mode a router config selects. Absent means `"model"`; anything but the
/// two declared modes is refused with the valid values in the message.
pub fn router_mode(config: &Value) -> Result<&'static str, String> {
    match config.get("mode") {
        None => Ok("model"),
        Some(Value::String(mode)) if mode == "model" => Ok("model"),
        Some(Value::String(mode)) if mode == "value" => Ok("value"),
        Some(Value::String(mode)) => Err(format!(
            "Router config field 'mode' has unknown value '{mode}'. Valid values: {}.",
            ROUTER_MODES.join(", ")
        )),
        Some(_) => Err(format!(
            "Router config field 'mode' must be a string. Valid values: {}.",
            ROUTER_MODES.join(", ")
        )),
    }
}

/// The mode-aware half of a router's config validation: the mode itself and,
/// for `value`, the whole deterministic shape (source, routes, default
/// route). Model-mode shape checks stay with `graph_node`'s Router arm, which
/// also consults the platform registry. Shared by `graph_node` add/update,
/// `graph validate` and import so all three refuse the same configs.
pub fn validate_router_config_shape(config: &Value) -> Result<&'static str, String> {
    let mode = router_mode(config)?;
    if mode == "value" {
        validate_value_router_config(config)?;
    }
    Ok(mode)
}

/// Parse and validate a `mode: "value"` router's config. The model router
/// fields are refused rather than ignored: `default_route` is a value
/// router's only default, so a `fallback` (which a model router would take
/// silently) must never reach the engine on this config.
pub fn validate_value_router_config(config: &Value) -> Result<ValueRouterConfig, String> {
    let map = config.as_object().ok_or_else(|| {
        "Graph node config must be a JSON object. Pass an object rather than a JSON-encoded string."
            .to_string()
    })?;
    for key in ["fallback", "decision", "min_confidence"] {
        if map.contains_key(key) {
            return Err(format!(
                "Router config field '{key}' applies to model routers only; remove it from a 'value' router."
            ));
        }
    }

    let source = map
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "Graph node config for kind 'router' with mode 'value' must include a non-empty 'source' field."
                .to_string()
        })?
        .to_string();
    if !VALUE_ROUTER_SOURCES.contains(&source.as_str()) {
        return Err(format!(
            "Router source '{source}' is not recognized. Valid sources: {}.",
            VALUE_ROUTER_SOURCES.join(", ")
        ));
    }

    let routes = parse_router_routes_array(map)?;
    let default_route = match map.get("default_route") {
        None => None,
        Some(raw) => Some(
            raw.as_str()
                .ok_or_else(|| {
                    "Graph node config field 'default_route' must be a string naming a declared route."
                        .to_string()
                })?
                .to_string(),
        ),
    };
    validate_value_router_routes(&routes, default_route.as_deref())?;
    Ok(ValueRouterConfig {
        source,
        routes,
        default_route,
    })
}

/// Parse a router's `routes` array from its config object. Shared by the
/// value-router shape check; a route's `description` is optional here
/// because a value router never shows routes to a model.
fn parse_router_routes_array(
    map: &serde_json::Map<String, Value>,
) -> Result<Vec<RouterRoute>, String> {
    let routes_value = map.get("routes").ok_or_else(|| {
        "Graph node config for kind 'router' must include a 'routes' array.".to_string()
    })?;
    let routes_array = routes_value
        .as_array()
        .ok_or_else(|| "Graph node config field 'routes' must be an array.".to_string())?;
    routes_array
        .iter()
        .map(|entry| {
            let obj = entry.as_object().ok_or_else(|| {
                "Each router route must be an object with a 'label' field.".to_string()
            })?;
            Ok(RouterRoute {
                label: obj
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                description: obj
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

/// Validate a value router's routes and optional default: `1`-`8` unique
/// non-empty labels (descriptions are optional), and a `default_route` that
/// names one of them. Pure config-shape check — no edges needed.
pub fn validate_value_router_routes(
    routes: &[RouterRoute],
    default_route: Option<&str>,
) -> Result<(), String> {
    if routes.len() < VALUE_ROUTER_MIN_ROUTES {
        return Err(format!(
            "A value router must declare at least {VALUE_ROUTER_MIN_ROUTES} route, got {}.",
            routes.len()
        ));
    }
    if routes.len() > ROUTER_MAX_ROUTES {
        return Err(format!(
            "A router must declare at most {ROUTER_MAX_ROUTES} routes, got {}.",
            routes.len()
        ));
    }

    let mut seen_labels = std::collections::HashSet::new();
    for route in routes {
        if route.label.trim().is_empty() {
            return Err("Every router route must have a non-empty label.".to_string());
        }
        if !seen_labels.insert(route.label.as_str()) {
            return Err(format!(
                "Router route label '{}' is declared more than once.",
                route.label
            ));
        }
    }

    if let Some(default) = default_route {
        if !routes.iter().any(|route| route.label == default) {
            return Err(format!(
                "Router default_route '{default}' does not name a declared route."
            ));
        }
    }

    Ok(())
}

/// Validate that every declared value-router route has exactly one outgoing
/// `route` edge from `from_node` — never zero (the run would dead-end) and
/// never two (the choice would be ambiguous). Callers pass each edge as
/// `(from_node, route_label)`, so the same rule serves the DB's
/// [`GraphEdge`]s and a shared document's name-referenced edges.
pub fn validate_value_router_route_coverage<'a>(
    routes: &[RouterRoute],
    from_node: &str,
    edges: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<(), String> {
    let labels: Vec<&str> = edges
        .into_iter()
        .filter(|(edge_from, _)| *edge_from == from_node)
        .filter_map(|(_, label)| label)
        .collect();
    for route in routes {
        let count = labels
            .iter()
            .filter(|label| **label == route.label.as_str())
            .count();
        if count != 1 {
            return Err(format!(
                "Router route '{}' must have exactly one outgoing edge, got {count}.",
                route.label
            ));
        }
    }
    Ok(())
}
