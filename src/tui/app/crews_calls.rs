//! CT30: worker-thread daemon I/O for the Crews face.
//!
//! Everything here runs on a background `std::thread` (D3) and talks only to
//! the daemon MCP tools — never to SQLite (D4). The UI side never blocks:
//! `crews_face.rs` spawns these and polls the channel from the refresh tick.

use std::collections::BTreeSet;

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

use super::crews_logic::{
    apply_member_edit, parse_crews, parse_pair_summaries, parse_roster, CrewBindingView,
    CrewsSnapshot, MemberEdit, RosterWrite,
};
use crate::tui::mcp_client::call_daemon_tool;

/// Call one daemon tool and parse its text payload as JSON. Tool-level
/// errors (`is_error`) abort with the daemon's message prefixed by the tool
/// name so the status line says which call failed.
fn call(port: &str, tool: &str, arguments: &Value) -> Result<Value> {
    let outcome = call_daemon_tool(port, tool, arguments)?;
    if outcome.is_error {
        bail!("{tool}: {}", outcome.text);
    }
    serde_json::from_str::<Value>(&outcome.text)
        .map_err(|error| anyhow!("{tool}: response is not JSON: {error}"))
}

/// One refresh's worth of data: `harness roster_list`, then the pair summaries,
/// then the crews (D8: exactly one `harness roster_list`, no per-pair checks).
pub(crate) fn fetch_snapshot(port: &str) -> Result<CrewsSnapshot> {
    let roster = parse_roster(&call(
        port,
        "harness",
        &serde_json::json!({ "action": "roster_list" }),
    )?)?;
    let pairs = parse_pair_summaries(&call(
        port,
        "crew",
        &serde_json::json!({ "action": "assignments", "group_by": "pair" }),
    )?)?;
    let crews = parse_crews(&call(
        port,
        "crew",
        &serde_json::json!({ "action": "list" }),
    )?)?;
    Ok(CrewsSnapshot {
        roster,
        pairs,
        crews,
    })
}

/// The real bindings of one crew (D16): `crew get` returns the `bindings`
/// array the `crew list` snapshot only counts.
pub(crate) fn fetch_crew_bindings(port: &str, name: &str) -> Result<Vec<CrewBindingView>> {
    let value = call(
        port,
        "crew",
        &serde_json::json!({ "action": "get", "name": name }),
    )?;
    let items = value
        .get("bindings")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("crew get: 'bindings' array missing"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(CrewBindingView {
            graph: item
                .get("graph")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: item
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(out)
}

/// Run the planned roster writes in order; the first error aborts the
/// batch. Returns the last tool's text payload.
pub(crate) fn run_roster_writes(port: &str, writes: &[RosterWrite]) -> Result<String> {
    let mut last = String::new();
    for write in writes {
        let (tool, arguments) = write.to_call();
        let value = call(port, tool, &arguments)?;
        last = value.to_string();
    }
    Ok(last)
}

/// The result of a successful crew member edit: the new version plus the
/// graph names any binding lives in (D15's running-note input; `crew get`'s
/// `bindings[].graph` carries the graph NAME — verified against
/// `graph_name_for_scope`).
#[derive(Debug, Clone)]
pub(crate) struct CrewWriteOutcome {
    pub version: i64,
    pub bound_graph_ids: Vec<String>,
}

/// Apply one member edit to a crew (D6): re-read the FRESH member list
/// from `crew list`, mutate it, then `crew update` replaces the whole list.
pub(crate) fn run_member_edit(
    port: &str,
    crew: &str,
    edit: &MemberEdit,
) -> Result<CrewWriteOutcome> {
    let list = call(port, "crew", &serde_json::json!({ "action": "list" }))?;
    let items = list
        .as_array()
        .ok_or_else(|| anyhow!("crew list: expected a JSON array"))?;
    let fresh = items
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(crew))
        .ok_or_else(|| anyhow!("crew '{crew}' not found"))?;
    let mut members = fresh
        .get("members")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    apply_member_edit(&mut members, edit).map_err(|error| anyhow!(error))?;

    let updated = call(
        port,
        "crew",
        &serde_json::json!({ "action": "update", "name": crew, "members": members }),
    )?;
    let version = updated
        .get("version")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("crew update: 'version' missing"))?;

    let bindings = fetch_crew_bindings(port, crew)?;
    let bound_graph_ids: Vec<String> = bindings
        .into_iter()
        .map(|binding| binding.graph)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(CrewWriteOutcome {
        version,
        bound_graph_ids,
    })
}
