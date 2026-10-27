//! `canopy graph validate`: check a shared graph or node document without
//! importing it — no database, no daemon round trip, since the report needs
//! nothing the file itself does not contain.

use anyhow::{anyhow, Result};

/// Validate one file, print the report (`--json` for the machine shape),
/// and fail the process when the document is invalid.
pub(super) fn handle_graph_validate(path: &str, json: bool) -> Result<()> {
    let report = crate::daemon::graph_validate::validate_request(Some(path.to_string()), None)
        .map_err(|e| anyhow!("'{path}': {e}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_validate_report(&report);
    }
    if report.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        Ok(())
    } else {
        let count = report
            .get("problems")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        Err(anyhow!(
            "'{path}' is not a valid shared document ({count} problem{}).",
            if count == 1 { "" } else { "s" }
        ))
    }
}

fn print_validate_report(report: &serde_json::Value) {
    if report.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        let kind = report
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let name = report
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let count = |key: &str| {
            report
                .get(key)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        println!(
            "ok: {} '{name}' is valid ({} nodes, {} edges, {} ensembles).",
            kind,
            count("nodes"),
            count("edges"),
            count("ensembles")
        );
        if let Some(needs) = report
            .get("needs_harness")
            .and_then(serde_json::Value::as_array)
            .filter(|needs| !needs.is_empty())
        {
            let names: Vec<&str> = needs.iter().filter_map(serde_json::Value::as_str).collect();
            println!("needs_harness: {}", names.join(", "));
        }
    } else if let Some(problems) = report.get("problems").and_then(serde_json::Value::as_array) {
        for problem in problems {
            let path = problem
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let message = problem
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            println!("{path}: {message}");
        }
    }
}
