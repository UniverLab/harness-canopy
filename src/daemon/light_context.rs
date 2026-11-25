use serde_json::{Map, Value};

const MAX_LIGHT_CONTEXT_CHARS: usize = 12_000;
const NODE_LIST_CAP: usize = 5;
const BODY_EXCERPT_MAX_CHARS: usize = 300;
const TITLE_MAX_CHARS: usize = 120;
const MESSAGE_MAX_CHARS: usize = 200;
const PAYLOAD_MAX_CHARS: usize = 300;
const LIGHT_HINT: &str =
    "light scope returns excerpts; read a node in full with intelligence_search";

const LIST_KEYS: [&str; 4] = ["sessions", "knowledge", "project_knowledge", "activity"];

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…", &s[..idx]),
        None => s.to_string(),
    }
}

fn truncate_string_field(obj: &mut Map<String, Value>, key: &str, max: usize) {
    if let Some(Value::String(s)) = obj.get_mut(key) {
        let truncated = truncate_chars(s, max);
        *s = truncated;
    }
}

fn shrink_node_list(arr: &mut Vec<Value>, cap: Option<usize>) -> usize {
    let removed = match cap {
        Some(limit) if arr.len() > limit => {
            let count = arr.len() - limit;
            arr.truncate(limit);
            count
        }
        _ => 0,
    };

    for item in arr.iter_mut() {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        if let Some(body) = obj.remove("body") {
            let excerpt = match body {
                Value::String(s) => Value::String(truncate_chars(&s, BODY_EXCERPT_MAX_CHARS)),
                other => other,
            };
            obj.insert("body_excerpt".to_string(), excerpt);
        }
        obj.remove("metadata");
        truncate_string_field(obj, "title", TITLE_MAX_CHARS);
    }

    removed
}

fn shrink_event_list(arr: &mut [Value]) {
    for item in arr.iter_mut() {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        truncate_string_field(obj, "message", MESSAGE_MAX_CHARS);
        truncate_string_field(obj, "payload", PAYLOAD_MAX_CHARS);
    }
}

fn increment_truncated(obj: &mut Map<String, Value>, key: &str, amount: usize) {
    if amount == 0 {
        return;
    }
    let truncated = obj
        .entry("truncated")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(map) = truncated.as_object_mut() {
        let prev = map.get(key).and_then(Value::as_u64).unwrap_or(0);
        map.insert(key.to_string(), Value::from(prev + amount as u64));
    }
}

fn pop_longest_list(obj: &mut Map<String, Value>) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for &key in &LIST_KEYS {
        let len = obj.get(key).and_then(Value::as_array).map_or(0, Vec::len);
        if len > best.map_or(0, |(_, best_len)| best_len) {
            best = Some((key, len));
        }
    }
    let best_key = best.map(|(key, _)| key)?;
    if let Some(arr) = obj.get_mut(best_key).and_then(Value::as_array_mut) {
        arr.pop();
    }
    Some(best_key)
}

fn enforce_budget(out: &mut Value) {
    loop {
        let len = serde_json::to_string_pretty(out).map_or(0, |s| s.chars().count());
        if len <= MAX_LIGHT_CONTEXT_CHARS {
            break;
        }
        let Some(obj) = out.as_object_mut() else {
            break;
        };
        if let Some(key) = pop_longest_list(obj) {
            increment_truncated(obj, key, 1);
            continue;
        }
        if obj.remove("traversal").is_some() {
            continue;
        }
        break;
    }
}

pub(crate) fn shrink_light_context(out: &mut Value) {
    let Some(obj) = out.as_object_mut() else {
        return;
    };

    for (key, cap) in [
        ("sessions", None),
        ("knowledge", Some(NODE_LIST_CAP)),
        ("project_knowledge", Some(NODE_LIST_CAP)),
    ] {
        let removed = obj
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .map_or(0, |arr| shrink_node_list(arr, cap));
        increment_truncated(obj, key, removed);
    }

    if let Some(arr) = obj.get_mut("activity").and_then(Value::as_array_mut) {
        shrink_event_list(arr);
    }

    obj.insert("hint".to_string(), Value::String(LIGHT_HINT.to_string()));
    enforce_budget(out);
}

#[cfg(test)]
mod tests {
    use super::shrink_light_context;
    use serde_json::{json, Value};

    fn pretty_chars(val: &Value) -> usize {
        serde_json::to_string_pretty(val).unwrap().chars().count()
    }

    fn make_node(id: usize, title: &str, body: &str) -> Value {
        json!({
            "id": format!("node-{id}"),
            "title": title,
            "body": body,
            "metadata": { "extra": "dropped" }
        })
    }

    #[test]
    fn light_context_caps_large_project_knowledge_under_budget() {
        let big_body = "x".repeat(10_240);
        let project_knowledge: Vec<Value> = (0..40)
            .map(|i| make_node(i, &format!("PK {i}"), &big_body))
            .collect();
        let knowledge: Vec<Value> = (0..5)
            .map(|i| make_node(i, &format!("K {i}"), &big_body))
            .collect();
        let mut out = json!({
            "scope": "light",
            "knowledge": knowledge,
            "project_knowledge": project_knowledge,
        });

        shrink_light_context(&mut out);

        assert!(pretty_chars(&out) <= 12_000);
        assert_eq!(out["knowledge"].as_array().unwrap().len(), 5);
        assert_eq!(out["project_knowledge"].as_array().unwrap().len(), 5);
        assert_eq!(out["truncated"]["project_knowledge"], 35);
    }

    #[test]
    fn light_context_truncates_multibyte_body_on_char_boundary() {
        let mut out = json!({
            "knowledge": [make_node(1, "utf8", &"é".repeat(400))]
        });

        shrink_light_context(&mut out);

        let excerpt = out["knowledge"][0]["body_excerpt"].as_str().unwrap();
        assert_eq!(excerpt.chars().count(), 301);
        assert!(excerpt.ends_with('…'));
        assert!(out["knowledge"][0].get("body").is_none());
        assert!(out["knowledge"][0].get("metadata").is_none());
    }

    #[test]
    fn light_context_truncates_activity_payload_and_keeps_null() {
        let mut out = json!({
            "activity": [
                { "message": "ok", "payload": "p".repeat(5_000) },
                { "message": "ok", "payload": null }
            ]
        });

        shrink_light_context(&mut out);

        let payload0 = out["activity"][0]["payload"].as_str().unwrap();
        assert_eq!(payload0.chars().count(), 301);
        assert!(payload0.ends_with('…'));
        assert!(out["activity"][1]["payload"].is_null());
    }

    #[test]
    fn light_context_budget_loop_empties_lists_and_falls_back() {
        let huge = "a".repeat(11_000);
        let mut out = json!({
            "sessions": [make_node(1, &huge, "b")],
            "knowledge": [make_node(2, &huge, "b")],
            "project_knowledge": [make_node(3, &huge, "b")],
            "activity": [{ "message": huge, "payload": "p" }],
            "traversal": { "nodes": vec!["t".repeat(1_000); 20] }
        });

        shrink_light_context(&mut out);

        assert!(pretty_chars(&out) <= 12_000);
        for key in ["sessions", "knowledge", "project_knowledge", "activity"] {
            assert_eq!(out[key].as_array().unwrap().len(), 0);
            assert_eq!(out["truncated"][key], 1);
        }
    }

    #[test]
    fn light_context_small_fixture_keeps_elements_without_truncated() {
        let mut out = json!({
            "scope": "light",
            "sessions": [{ "id": "s1", "title": "Session 1", "body": "short body" }],
            "knowledge": [{ "id": "k1", "title": "Fact 1", "body": "short fact" }],
            "activity": [{ "message": "hello", "payload": "world" }]
        });

        shrink_light_context(&mut out);

        assert!(out.get("truncated").is_none());
        assert_eq!(
            out["hint"],
            "light scope returns excerpts; read a node in full with intelligence_search"
        );
        assert_eq!(out["sessions"][0]["body_excerpt"], "short body");
        assert!(out["sessions"][0].get("body").is_none());
        assert_eq!(out["knowledge"][0]["body_excerpt"], "short fact");
        assert_eq!(out["activity"][0]["message"], "hello");
        assert_eq!(out["activity"][0]["payload"], "world");
    }
}
