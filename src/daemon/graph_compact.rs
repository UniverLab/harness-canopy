//! Compact projection and item selection for `graph get`.
//!
//! Both functions operate on the full JSON that `graph_details_json` returns,
//! so the default `graph get` output stays byte-for-byte unchanged.

use serde_json::{json, Value};

const COMMAND_PREVIEW_CHARS: usize = 120;

/// Compact a full `graph_details_json` value into a summary that fits in agent
/// context. The result has exactly the keys documented by the `graph get`
/// `compact` parameter; no prompt template, member config, node config or node
/// env appears.
pub(crate) fn compact_graph_json(full: &Value) -> Value {
    let hooks = compact_hooks(full.get("hooks"));
    let graph = full.get("graph");
    let nodes = graph.and_then(|g| g.get("nodes")).and_then(Value::as_array);
    let edges = graph.and_then(|g| g.get("edges")).and_then(Value::as_array);
    let ensembles = graph
        .and_then(|g| g.get("ensembles"))
        .and_then(Value::as_array);
    let specs = full.get("specs").and_then(Value::as_array);

    json!({
        "id": full.get("id").cloned().unwrap_or(Value::Null),
        "name": full.get("name").cloned().unwrap_or(Value::Null),
        "status": full.get("status").cloned().unwrap_or(Value::Null),
        "workdir": full.get("workdir").cloned().unwrap_or(Value::Null),
        "blocked": full.get("blocked").cloned().unwrap_or(Value::Null),
        "blocker": full.get("blocker").cloned().unwrap_or(Value::Null),
        "trigger": full.get("trigger").cloned().unwrap_or(Value::Null),
        "hooks": hooks,
        "counts": {
            "nodes": nodes.map_or(0, Vec::len),
            "edges": edges.map_or(0, Vec::len),
            "ensembles": ensembles.map_or(0, Vec::len),
            "specs": specs.map_or(0, Vec::len),
        },
        "ensembles": ensembles.map_or_else(Vec::new, |arr| arr.iter().map(compact_ensemble).collect()),
        "nodes": nodes.map_or_else(Vec::new, |arr| arr.iter().map(compact_node).collect()),
        "edges": edges.map_or_else(Vec::new, |arr| arr.iter().filter_map(compact_edge).collect()),
        "specs": specs.map_or_else(Vec::new, |arr| arr.iter().map(compact_spec).collect()),
        "compact": true,
        "hint": "compact summary; read one item in full with node_id or ensemble",
    })
}

fn compact_hooks(hooks: Option<&Value>) -> Value {
    let Some(arr) = hooks.and_then(Value::as_array) else {
        return json!([]);
    };
    Value::Array(arr.iter().map(compact_hook_entry).collect())
}

fn compact_hook_entry(entry: &Value) -> Value {
    let event = entry.get("event").cloned().unwrap_or(Value::Null);
    let hooks = entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(compact_hook).collect::<Vec<_>>())
        .unwrap_or_default();
    json!({ "event": event, "hooks": hooks })
}

fn compact_hook(hook: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in [
        "platform",
        "crew",
        "command",
        "target_session_id",
        "target_session_name",
        "target_graph_id",
        "queue_id",
        "workdir_override",
    ] {
        if let Some(value) = hook.get(key) {
            if !value.is_null() {
                out.insert(key.to_string(), value.clone());
            }
        }
    }
    Value::Object(out)
}

fn compact_ensemble(ensemble: &Value) -> Value {
    let member_count = ensemble
        .get("members")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    json!({
        "id": ensemble.get("id").cloned().unwrap_or(Value::Null),
        "name": ensemble.get("name").cloned().unwrap_or(Value::Null),
        "kind": ensemble.get("kind").cloned().unwrap_or(Value::Null),
        "crew": ensemble.get("crew").cloned().unwrap_or(Value::Null),
        "member_count": member_count,
        "commit_rights": ensemble.get("commit_rights").cloned().unwrap_or(Value::Null),
    })
}

fn compact_node(node: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "id".to_string(),
        node.get("id").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "name".to_string(),
        node.get("name").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "kind".to_string(),
        node.get("kind").cloned().unwrap_or(Value::Null),
    );
    if let Some(command) = node
        .get("config")
        .and_then(Value::as_object)
        .and_then(|cfg| cfg.get("command"))
        .and_then(Value::as_str)
    {
        out.insert(
            "command".to_string(),
            Value::String(preview_command(command)),
        );
    }
    Value::Object(out)
}

fn preview_command(command: &str) -> String {
    let mut chars = command.chars();
    let preview: String = chars.by_ref().take(COMMAND_PREVIEW_CHARS).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn compact_edge(edge: &Value) -> Option<Value> {
    let from = edge.get("from_node")?.as_str()?;
    let to = edge.get("to_node")?.as_str()?;
    let condition = edge.get("condition")?.as_str()?;
    Some(Value::String(format!("{from} -> {to} [{condition}]")))
}

fn compact_spec(spec: &Value) -> Value {
    let node_count = spec
        .get("nodes")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let ensemble_count = spec
        .get("ensembles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    json!({
        "id": spec.get("id").cloned().unwrap_or(Value::Null),
        "name": spec.get("name").cloned().unwrap_or(Value::Null),
        "status": spec.get("status").cloned().unwrap_or(Value::Null),
        "node_count": node_count,
        "ensemble_count": ensemble_count,
    })
}

/// Select one node or ensemble from the full `graph_details_json` value.
/// `node_id` matches graph/spec nodes by id (exact, then unique prefix).
/// `ensemble` matches graph/spec ensembles by id (exact, then unique prefix) or
/// exact name. Returns the selected element unchanged, wrapped with `graph_id`.
pub(crate) fn select_graph_item(
    full: &Value,
    node_id: Option<&str>,
    ensemble: Option<&str>,
) -> Result<Value, String> {
    let graph_id = full
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if let Some(value) = node_id {
        let node = select_by_id(full, value, "node", graph_node_collections(full))?;
        return Ok(json!({ "graph_id": graph_id, "node": node }));
    }
    if let Some(value) = ensemble {
        let ens = select_ensemble(full, value, &graph_id)?;
        return Ok(json!({ "graph_id": graph_id, "ensemble": ens }));
    }
    Err("node_id or ensemble is required".to_string())
}

fn graph_node_collections(full: &Value) -> Vec<&[Value]> {
    let mut collections = Vec::new();
    if let Some(nodes) = full
        .get("graph")
        .and_then(|g| g.get("nodes"))
        .and_then(Value::as_array)
    {
        collections.push(nodes.as_slice());
    }
    if let Some(specs) = full.get("specs").and_then(Value::as_array) {
        for spec in specs {
            if let Some(nodes) = spec.get("nodes").and_then(Value::as_array) {
                collections.push(nodes.as_slice());
            }
        }
    }
    collections
}

fn graph_ensemble_collections(full: &Value) -> Vec<&[Value]> {
    let mut collections = Vec::new();
    if let Some(ensembles) = full
        .get("graph")
        .and_then(|g| g.get("ensembles"))
        .and_then(Value::as_array)
    {
        collections.push(ensembles.as_slice());
    }
    if let Some(specs) = full.get("specs").and_then(Value::as_array) {
        for spec in specs {
            if let Some(ensembles) = spec.get("ensembles").and_then(Value::as_array) {
                collections.push(ensembles.as_slice());
            }
        }
    }
    collections
}

fn select_by_id(
    full: &Value,
    value: &str,
    kind: &str,
    collections: Vec<&[Value]>,
) -> Result<Value, String> {
    let graph_id = full.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut exact = None;
    let mut prefix_matches: Vec<&Value> = Vec::new();
    for collection in collections {
        for item in collection {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            if id == value {
                exact = Some(item.clone());
                break;
            }
            if id.starts_with(value)
                && !prefix_matches
                    .iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == Some(id))
            {
                prefix_matches.push(item);
            }
        }
        if exact.is_some() {
            break;
        }
    }
    if let Some(item) = exact {
        return Ok(item);
    }
    match prefix_matches.len() {
        1 => Ok(prefix_matches[0].clone()),
        0 => Err(format!("{kind} '{value}' not found in graph {graph_id}")),
        _ => Err(format!("{kind} '{value}' matches more than one {kind}")),
    }
}

fn select_ensemble(full: &Value, value: &str, graph_id: &str) -> Result<Value, String> {
    let collections = graph_ensemble_collections(full);
    let mut exact_id = None;
    let mut prefix_matches: Vec<&Value> = Vec::new();
    let mut exact_name_matches: Vec<&Value> = Vec::new();
    for collection in &collections {
        for item in *collection {
            let id = item.get("id").and_then(Value::as_str);
            let name = item.get("name").and_then(Value::as_str);
            if id == Some(value) {
                exact_id = Some((*item).clone());
                break;
            }
            if id.is_some_and(|candidate| {
                candidate.starts_with(value)
                    && !prefix_matches
                        .iter()
                        .any(|m| m.get("id").and_then(Value::as_str) == Some(candidate))
            }) {
                prefix_matches.push(item);
            }
            if name == Some(value)
                && !exact_name_matches
                    .iter()
                    .any(|m| m.get("id").and_then(Value::as_str) == id)
            {
                exact_name_matches.push(item);
            }
        }
        if exact_id.is_some() {
            break;
        }
    }
    if let Some(item) = exact_id {
        return Ok(item);
    }
    match prefix_matches.len() {
        1 => return Ok(prefix_matches[0].clone()),
        n if n > 1 => return Err(format!("ensemble '{value}' matches more than one ensemble")),
        _ => {}
    }
    match exact_name_matches.len() {
        1 => Ok(exact_name_matches[0].clone()),
        0 => Err(format!("ensemble '{value}' not found in graph {graph_id}")),
        _ => Err(format!("ensemble '{value}' matches more than one ensemble")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big_text(n: usize) -> String {
        "x".repeat(n)
    }

    fn node_json(id: &str, name: &str, with_command: bool) -> Value {
        let mut config = serde_json::Map::new();
        config.insert("prompt".to_string(), Value::String(big_text(2000)));
        config.insert("prompt_template".to_string(), Value::String(big_text(2000)));
        if with_command {
            config.insert(
                "command".to_string(),
                Value::String("echo hello".to_string()),
            );
        }
        json!({
            "id": id,
            "spec_id": "spec-1",
            "graph_id": "g1",
            "name": name,
            "kind": "agent",
            "config": Value::Object(config),
            "position": 0,
            "created_at": "2026-01-01T00:00:00Z",
            "routes": null,
            "prompt_source": "explicit",
            "infra_config": null,
        })
    }

    fn member_json(node_id: &str, pos: usize) -> Value {
        json!({
            "node_id": node_id,
            "position": pos,
            "platform": "p",
            "model": "m",
            "prompt_override": big_text(500),
            "prompt_source": "override",
            "infra_retry_limit_override": null,
            "infra_crash_max_seconds_override": null,
            "infra_backoff_seconds_override": null,
        })
    }

    fn ensemble_json(id: &str, name: &str, member_count: usize) -> Value {
        let members: Vec<Value> = (0..member_count)
            .map(|i| member_json(&format!("{id}-m{i}"), i))
            .collect();
        json!({
            "id": id,
            "name": name,
            "kind": "router",
            "commit_rights": false,
            "read_only": true,
            "mutex": null,
            "prompt_template": big_text(2000),
            "join_node_id": format!("{id}-join"),
            "entry_from_node": "n0",
            "entry_condition": "always",
            "entry_sources": [],
            "min_pass": 1,
            "straggler_timeout_minutes": null,
            "effective_straggler_timeout_minutes": null,
            "quorum_grace_minutes": null,
            "timeout_minutes": 30,
            "infra_retry_limit": null,
            "infra_crash_max_seconds": null,
            "infra_backoff_seconds": null,
            "on_pass_to": null,
            "on_fail_to": null,
            "round_robin_index": 0,
            "crew": "my-crew",
            "crew_version_applied": null,
            "created_at": "2026-01-01T00:00:00Z",
            "members": members,
        })
    }

    fn edge_json(from: &str, to: &str) -> Value {
        json!({
            "id": format!("{from}-{to}"),
            "spec_id": "spec-1",
            "graph_id": "g1",
            "from_node": from,
            "to_node": to,
            "condition": "pass",
            "route": "pass",
        })
    }

    fn spec_json(id: &str, name: &str, nodes: &[Value], ensembles: &[Value]) -> Value {
        json!({
            "id": id,
            "graph_id": "g1",
            "name": name,
            "type": "feat",
            "scope": null,
            "code": null,
            "commit_message": null,
            "description": big_text(500),
            "position": 0,
            "parallelizable": false,
            "status": "pending",
            "current_node": null,
            "blocked": false,
            "blocker": null,
            "resume_actions": [],
            "spec_start_head": null,
            "spec_committed_head": null,
            "spec_start_dirty": null,
            "spec_end_dirty": null,
            "spec_end_dirty_paths": null,
            "started_at": null,
            "completed_at": null,
            "nodes": nodes,
            "edges": [],
            "ensembles": ensembles,
            "runs": [],
        })
    }

    fn fixture() -> Value {
        let mut nodes = Vec::new();
        for i in 0..50 {
            nodes.push(node_json(
                &format!("n{i}"),
                &format!("Node {i}"),
                i % 10 == 0,
            ));
        }
        let mut ensembles = Vec::new();
        for i in 0..7 {
            ensembles.push(ensemble_json(&format!("e{i}"), &format!("Ens {i}"), 6));
        }
        let mut edges = Vec::new();
        for i in 0..49 {
            edges.push(edge_json(&format!("n{i}"), &format!("n{}", i + 1)));
        }
        let specs = vec![
            spec_json("s1", "Spec 1", &[node_json("sn1", "SN1", false)], &[]),
            spec_json("s2", "Spec 2", &[], &[ensemble_json("se1", "SE1", 2)]),
            spec_json("s3", "Spec 3", &[], &[]),
        ];
        json!({
            "id": "g1",
            "name": "Big graph",
            "description": big_text(1000),
            "workdir": "/tmp/w",
            "status": "running",
            "blocked": false,
            "blocker": null,
            "trigger": null,
            "trigger_prompt": null,
            "created_at": "2026-01-01T00:00:00Z",
            "started_at": null,
            "completed_at": null,
            "autorun_at": null,
            "archived": false,
            "require_spec_type": false,
            "graph": {
                "nodes": nodes,
                "edges": edges,
                "ensembles": ensembles,
            },
            "specs": specs,
            "on_completed": null,
            "hooks": [{
                "event": "on_completed",
                "hooks": [{
                    "platform": "p",
                    "crew": "c",
                    "model": "m",
                    "effort": "high",
                    "prompt": "secret prompt",
                    "command": "echo hi",
                    "target_session_id": "ses1",
                    "target_session_name": "name",
                    "timeout_minutes": 30,
                    "target_graph_id": "g2",
                    "queue_id": "q1",
                    "workdir_override": "/tmp/x",
                    "idea": "idea text",
                }]
            }],
            "completion_hook_runs": [],
            "orphan_join_warnings": [],
        })
    }

    fn has_key_recursive(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(map) => {
                if map.contains_key(key) {
                    return true;
                }
                map.values().any(|v| has_key_recursive(v, key))
            }
            Value::Array(arr) => arr.iter().any(|v| has_key_recursive(v, key)),
            _ => false,
        }
    }

    #[test]
    fn compact_is_small_and_has_exact_keys() {
        let full = fixture();
        let compact = compact_graph_json(&full);
        let pretty = serde_json::to_string_pretty(&compact).unwrap();
        assert!(pretty.len() < 15_000, "compact too large: {}", pretty.len());

        let obj = compact.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "blocked",
                "blocker",
                "compact",
                "counts",
                "edges",
                "ensembles",
                "hint",
                "hooks",
                "id",
                "name",
                "nodes",
                "specs",
                "status",
                "trigger",
                "workdir",
            ]
        );
        assert_eq!(compact["compact"], true);
        assert!(!has_key_recursive(&compact, "prompt_template"));
        assert!(!has_key_recursive(&compact, "config"));
        assert!(!has_key_recursive(&compact, "prompt_override"));

        // hook prompt absent, kept keys present
        let hook = &compact["hooks"][0]["hooks"][0];
        let hook_obj = hook.as_object().unwrap();
        assert!(!hook_obj.contains_key("prompt"));
        assert!(!hook_obj.contains_key("model"));
        assert!(!hook_obj.contains_key("effort"));
        assert!(!hook_obj.contains_key("timeout_minutes"));
        assert!(!hook_obj.contains_key("idea"));
        assert_eq!(hook["command"], "echo hi");
        assert_eq!(hook["platform"], "p");
    }

    #[test]
    fn compact_counts_and_shapes() {
        let full = fixture();
        let compact = compact_graph_json(&full);
        assert_eq!(compact["counts"]["nodes"], 50);
        assert_eq!(compact["counts"]["edges"], 49);
        assert_eq!(compact["counts"]["ensembles"], 7);
        assert_eq!(compact["counts"]["specs"], 3);
        assert_eq!(compact["ensembles"][0]["member_count"], 6);
        assert_eq!(compact["ensembles"][0]["crew"], "my-crew");
        assert_eq!(compact["edges"][0], "n0 -> n1 [pass]");
        assert_eq!(compact["specs"][0]["node_count"], 1);
        assert_eq!(compact["specs"][1]["ensemble_count"], 1);
        // command preview
        assert_eq!(compact["nodes"][0]["command"], "echo hello");
        assert!(compact["nodes"][1].get("command").is_none());
    }

    #[test]
    fn compact_command_truncates_on_char_boundary() {
        let mut node = node_json("n1", "N1", false);
        let long = "é".repeat(130); // multibyte
        node["config"]["command"] = Value::String(long);
        let compact = compact_node(&node);
        let cmd = compact["command"].as_str().unwrap();
        assert!(cmd.ends_with('…'));
        // 120 chars of é + ellipsis
        assert_eq!(cmd.chars().count(), 121);
    }

    #[test]
    fn select_node_by_id_prefix_and_not_found() {
        let full = fixture();
        let by_id = select_graph_item(&full, Some("n12"), None).unwrap();
        assert_eq!(by_id["graph_id"], "g1");
        assert_eq!(by_id["node"]["id"], "n12");
        assert!(by_id["node"].get("config").is_some());

        let by_prefix = select_graph_item(&full, Some("sn1"), None).unwrap();
        assert_eq!(by_prefix["node"]["id"], "sn1");

        let err = select_graph_item(&full, Some("missing"), None).unwrap_err();
        assert_eq!(err, "node 'missing' not found in graph g1");
    }

    #[test]
    fn select_node_ambiguous_prefix() {
        let full = fixture();
        // "n" matches n0..n49 (and no exact id is "n") - ambiguous
        let err = select_graph_item(&full, Some("n"), None).unwrap_err();
        assert_eq!(err, "node 'n' matches more than one node");
    }

    #[test]
    fn select_ensemble_by_id_prefix_name_and_errors() {
        let full = fixture();
        let by_id = select_graph_item(&full, None, Some("e2")).unwrap();
        assert_eq!(by_id["ensemble"]["id"], "e2");
        assert!(by_id["ensemble"].get("prompt_template").is_some());

        let by_prefix = select_graph_item(&full, None, Some("se1")).unwrap();
        assert_eq!(by_prefix["ensemble"]["id"], "se1");

        let by_name = select_graph_item(&full, None, Some("Ens 3")).unwrap();
        assert_eq!(by_name["ensemble"]["id"], "e3");

        let err = select_graph_item(&full, None, Some("nope")).unwrap_err();
        assert_eq!(err, "ensemble 'nope' not found in graph g1");
    }

    #[test]
    fn select_ensemble_ambiguous_prefix() {
        let full = fixture();
        // e1 matches e1..e6? e1, e2,e3,e4,e5,e6 - "e1" exact matches e1; "e" prefix matches all e0-e6
        let err = select_graph_item(&full, None, Some("e")).unwrap_err();
        assert_eq!(err, "ensemble 'e' matches more than one ensemble");
    }
}
