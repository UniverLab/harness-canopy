//! `canopy graph list --json` / `--running` — the script-friendly read over
//! the daemon's `graph` `list` MCP action.
//!
//! Install scripts used to open `background_agents.db` directly to learn
//! whether a graph is running, and silently broke once when a table was
//! renamed. This module gives them a supported way to ask: the exact same
//! MCP shape `graph list` already returns, projected down to the six stable
//! script-facing keys. It never opens the database — and never starts the
//! daemon either, since starting one runs recovery that kills live graph
//! runs. When the daemon is unreachable it exits 3 with `daemon not
//! reachable` on stderr and nothing on stdout.

use anyhow::{Context, Result};

use super::GraphAction;
use crate::daemon::cli_daemon::call_tool;

/// Line printed on stderr when the daemon cannot be reached. Scripts match
/// on this line, so keep it byte-stable.
pub(super) const DAEMON_UNREACHABLE_MESSAGE: &str = "daemon not reachable";

/// Process exit code when the daemon cannot be reached.
pub(super) const DAEMON_UNREACHABLE_EXIT_CODE: i32 = 3;

/// Classify a list failure: a transport failure (the daemon isn't listening
/// on the resolved port) maps to [`DAEMON_UNREACHABLE_EXIT_CODE`]; anything
/// else — a tool-level rejection, a malformed response — propagates as a
/// normal error. Split out so tests can assert the mapping without calling
/// [`std::process::exit`], which cannot run inside a test.
pub(super) fn daemon_exit_code(err: &anyhow::Error) -> Option<i32> {
    err.chain()
        .any(|cause| cause.to_string().contains("could not reach the daemon"))
        .then_some(DAEMON_UNREACHABLE_EXIT_CODE)
}

/// Answer `list --json`/`--running` through the daemon without touching
/// the database — and without starting the daemon, since starting one runs
/// recovery that kills live graph runs. Returns `None` for every other
/// action (including a plain `list`, which keeps its table rendering), so
/// the caller falls through to the database-backed handlers.
pub(super) fn try_handle_daemon_list(
    action: &GraphAction,
    port_override: Option<u16>,
) -> Option<Result<()>> {
    let GraphAction::List {
        workdir,
        json,
        running,
    } = action
    else {
        return None;
    };
    if !json && !running {
        return None;
    }
    Some(handle_list_via_daemon(
        port_override,
        workdir.as_deref(),
        *running,
        *json,
    ))
}

/// Ask the daemon for its `graph` `list` result and print it as one JSON
/// array on stdout and nothing else.
///
/// Each item carries exactly `id`, `name`, `status`, `workdir`,
/// `current_spec` and `current_node` (the last two null when the graph has
/// no current spec or node). `running` keeps only graphs whose status is
/// `running`. An empty result prints `[]`. `--json` prints the readable
/// multi-line form (the same convention as `graph crew --json`); `--running`
/// alone prints the compact single-line form scripts consume.
pub(super) fn handle_list_via_daemon(
    port_override: Option<u16>,
    workdir: Option<&str>,
    running: bool,
    json: bool,
) -> Result<()> {
    let mut args = serde_json::json!({ "action": "list" });
    if let Some(workdir) = workdir {
        args["workdir"] = serde_json::json!(workdir);
    }
    let text = match call_tool(port_override, "graph", &args) {
        Ok(text) => text,
        Err(err) if daemon_exit_code(&err).is_some() => {
            eprintln!("{DAEMON_UNREACHABLE_MESSAGE}");
            std::process::exit(DAEMON_UNREACHABLE_EXIT_CODE);
        }
        Err(err) => return Err(err),
    };
    let items = project_graph_list(&text, running)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        println!("{}", serde_json::to_string(&items)?);
    }
    Ok(())
}

/// Parse the daemon's `graph` `list` text (a JSON array in the full MCP
/// summary shape) and project each entry down to the six stable
/// script-facing keys, dropping everything else. Anything that is not a
/// JSON array is an error, never an empty list: a valid-JSON non-array
/// reading as `[]` would tell scripts "nothing is running" when the real
/// answer is "the response shape changed".
fn project_graph_list(text: &str, running: bool) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_str(text)
        .with_context(|| format!("unexpected graph list response: {text}"))?;
    let items = value
        .as_array()
        .cloned()
        .with_context(|| format!("unexpected graph list response: {text}"))?;
    Ok(items
        .into_iter()
        .filter(|item| {
            !running || item.get("status").and_then(|s| s.as_str()) == Some("running")
        })
        .map(|item| {
            serde_json::json!({
                "id": item.get("id").cloned().unwrap_or(serde_json::Value::Null),
                "name": item.get("name").cloned().unwrap_or(serde_json::Value::Null),
                "status": item.get("status").cloned().unwrap_or(serde_json::Value::Null),
                "workdir": item.get("workdir").cloned().unwrap_or(serde_json::Value::Null),
                "current_spec": item.get("current_spec").cloned().unwrap_or(serde_json::Value::Null),
                "current_node": item.get("current_node").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect())
}
