use anyhow::Result;
use std::collections::BTreeMap;

pub(super) const WRAPPER_KEYS: [&str; 4] = ["mcpServers", "servers", "mcp_servers", "mcp"];

/// Parse pasted README JSON into (name, entry) pairs.
///
/// Accepts, in order: (a) an object with a top-level wrapper key
/// (`mcpServers`, `servers`, `mcp_servers`, `mcp`) mapping to name → entry;
/// (b) an object that is itself name → entry; (c) a fragment without outer
/// braces, wrapped in `{` `}` and re-processed as (a)/(b). Entries are kept
/// verbatim. An entry is valid when it is an object with a string `command`
/// or a string `url`.
pub fn parse_pasted_servers(text: &str) -> Result<Vec<(String, serde_json::Value)>> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => extract_servers_from_value(&value),
        Err(first_error) => {
            let trimmed = text.trim().trim_end_matches(',').trim();
            let wrapped = format!("{{{trimmed}}}");
            match serde_json::from_str::<serde_json::Value>(&wrapped) {
                Ok(value) => extract_servers_from_value(&value),
                Err(_) => Err(first_error.into()),
            }
        }
    }
}

pub(super) fn extract_servers_from_value(
    value: &serde_json::Value,
) -> Result<Vec<(String, serde_json::Value)>> {
    let Some(obj) = value.as_object() else {
        return Err(anyhow::anyhow!("no servers found: expected a JSON object"));
    };

    let mut candidate: Option<&serde_json::Map<String, serde_json::Value>> = None;
    for key in WRAPPER_KEYS {
        if let Some(map) = obj.get(key).and_then(|v| v.as_object()) {
            candidate = Some(map);
            break;
        }
    }
    // serde_json::Map preserves insertion order but BTreeMap-style key order
    // is what tests assert; collect via the map's own iteration which for
    // serde_json (preserve_order feature) is insertion order — normalize by
    // sorting keys so output is deterministic regardless of input order.
    let pairs: Vec<(String, serde_json::Value)> = match candidate {
        Some(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        None => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    };

    let mut servers: Vec<(String, serde_json::Value)> = Vec::new();
    for (name, entry) in pairs {
        let valid = entry.as_object().is_some_and(|o| {
            o.get("command").and_then(|c| c.as_str()).is_some()
                || o.get("url").and_then(|u| u.as_str()).is_some()
        });
        if !valid {
            return Err(anyhow::anyhow!(
                "server '{name}' needs `command` or `url`: entry must be an object with a string `command` or `url`"
            ));
        }
        servers.push((name, entry));
    }

    if servers.is_empty() {
        return Err(anyhow::anyhow!("no servers found: no name → entry pairs"));
    }

    servers.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(servers)
}

/// Split a command line into (command, args), honoring shell quoting.
pub fn split_command_line(line: &str) -> Result<(String, Vec<String>)> {
    let parts = shell_words::split(line)?;
    let mut parts = parts.into_iter();
    match parts.next() {
        Some(command) if !command.is_empty() => Ok((command, parts.collect())),
        _ => Err(anyhow::anyhow!("Command is required.")),
    }
}

/// Parse `KEY=VALUE` tokens (shell-quoted) into a map.
pub fn parse_kv_map(text: &str) -> Result<BTreeMap<String, String>> {
    if text.trim().is_empty() {
        return Ok(BTreeMap::new());
    }

    let mut map = BTreeMap::new();
    for token in shell_words::split(text)? {
        let Some((key, value)) = token.split_once('=') else {
            return Err(anyhow::anyhow!("'{token}' must be KEY=VALUE (missing '=')"));
        };
        if key.is_empty() {
            return Err(anyhow::anyhow!("'{token}' must be KEY=VALUE (empty key)"));
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

/// Display-only type for a pasted entry: `stdio` when it has `command`, else `http`.
pub fn infer_display_type(entry: &serde_json::Value) -> &'static str {
    if entry.get("command").and_then(|c| c.as_str()).is_some() {
        "stdio"
    } else {
        "http"
    }
}
