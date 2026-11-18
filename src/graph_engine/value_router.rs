use super::*;
use crate::domain::graphs::validate_value_router_config;

/// Read the text a `mode: "value"` router routes on. `spec_type`,
/// `spec_scope` and `spec_tier` read from the current spec (empty when the
/// spec has none); `previous_output` is the previous node's output as the
/// data a router reads: a check node's trimmed stdout, or — for any other
/// node — the same text a model router receives as its input. All values are
/// trimmed; an entry router (no previous output) reads the empty string.
///
/// The source set is [`crate::domain::graphs::VALUE_ROUTER_SOURCES`]; adding
/// one there without a read here is an error rather than a silent empty
/// value (which would route on `""` instead of the new source).
pub(crate) fn read_value_source(
    source: &str,
    spec: &GraphSpec,
    previous_output: Option<&Value>,
) -> Result<String> {
    match source {
        "spec_type" => Ok(spec
            .spec_type
            .map(|spec_type| spec_type.as_str().to_string())
            .unwrap_or_default()),
        "spec_scope" => Ok(spec.spec_scope.clone().unwrap_or_default()),
        "spec_tier" => Ok(spec
            .review_tier
            .map(|tier| tier.as_str().to_string())
            .unwrap_or_default()),
        "previous_output" => Ok(previous_output_text(previous_output)),
        other => bail!("value router source '{other}' has no reader in the engine"),
    }
}

/// The previous node's output reduced to the text a value router compares:
/// a check node contributes its `stdout`, every other node the pretty JSON a
/// model router would be handed as its input. `None` is the empty string, so
/// an entry router's no-match rule is the ordinary one.
pub(crate) fn previous_output_text(previous_output: Option<&Value>) -> String {
    let Some(output) = previous_output else {
        return String::new();
    };
    if output.get("kind").and_then(Value::as_str) == Some("check") {
        return output
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
    }
    serde_json::to_string_pretty(output)
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Execute a deterministic (`mode: "value"`) router: read the configured
/// source, match it case-sensitively against the route labels, and follow
/// that route's edge. No platform, model, crew or agent process is resolved
/// or spawned — this path works on a graph with no platform configured. A
/// value that matches no route takes the declared `default_route`, or fails
/// the node outright; it never takes a silent fallback.
pub(crate) fn execute_value_router_node(
    spec: &GraphSpec,
    node: &GraphNode,
    previous_output: Option<&Value>,
) -> Result<NodeExecution> {
    let routing = validate_value_router_config(&node.config)
        .map_err(|message| anyhow!("Router node '{}': {message}", node.name))?;
    let value = read_value_source(&routing.source, spec, previous_output)
        .map_err(|error| anyhow!("Router node '{}': {error}", node.name))?;

    let matched = routing
        .routes
        .iter()
        .find(|route| route.label == value)
        .map(|route| route.label.as_str());
    let (route, used_default) = match matched {
        Some(label) => (label.to_string(), false),
        None => match routing.default_route.clone() {
            Some(default) => (default, true),
            None => return Ok(no_match_execution(node, &routing, &value)),
        },
    };

    tracing::info!(
        node = %node.name,
        source = %routing.source,
        value = %value,
        route = %route,
        used_default,
        "value router node decided route"
    );

    Ok(NodeExecution {
        status: GraphRunStatus::Pass,
        output: serde_json::json!({
            "kind": "router",
            "mode": "value",
            "node_id": node.id,
            "source": &routing.source,
            "value": &value,
            "route": &route,
            "used_default": used_default,
        }),
        summary: format!(
            "Router node '{}' selected route '{}' (value '{}' from {}){}.",
            node.name,
            route,
            value,
            routing.source,
            if used_default { " (default)" } else { "" }
        ),
    })
}

/// The failure execution for a value that matched no route and had no
/// `default_route`: the spec's exact operator-facing message, recorded on
/// the run row so the choice is auditable.
fn no_match_execution(
    node: &GraphNode,
    routing: &crate::domain::graphs::ValueRouterConfig,
    value: &str,
) -> NodeExecution {
    let routes = routing
        .routes
        .iter()
        .map(|route| route.label.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let message = format!(
        "router '{}': value '{value}' from {} matches no route (routes: {routes})",
        node.name, routing.source
    );
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "router",
            "mode": "value",
            "node_id": node.id,
            "source": &routing.source,
            "value": value,
            "error": &message,
        }),
        summary: message,
    }
}
