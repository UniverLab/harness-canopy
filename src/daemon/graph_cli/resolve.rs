use anyhow::{anyhow, Result};

use super::format::short_id;
use crate::domain::graphs::Graph;

pub(super) fn resolve_graph<'a>(graphs: &'a [Graph], query: &str) -> Result<&'a Graph> {
    let query = query.trim();
    if query.is_empty() {
        return Err(anyhow!("Graph id or name must not be empty."));
    }

    if let Some(lp) = graphs.iter().find(|l| l.id == query) {
        return Ok(lp);
    }

    let name_matches: Vec<&Graph> = graphs.iter().filter(|l| l.name == query).collect();
    match name_matches.len() {
        1 => return Ok(name_matches[0]),
        n if n > 1 => return Err(ambiguous_error(query, &name_matches)),
        _ => {}
    }

    let prefix_matches: Vec<&Graph> = graphs.iter().filter(|l| l.id.starts_with(query)).collect();
    match prefix_matches.len() {
        1 => Ok(prefix_matches[0]),
        0 => Err(not_found_error(query, graphs)),
        _ => Err(ambiguous_error(query, &prefix_matches)),
    }
}

pub(super) fn not_found_error(query: &str, all: &[Graph]) -> anyhow::Error {
    if all.is_empty() {
        return anyhow!("No graph matches '{query}' — no graphs exist yet.");
    }
    anyhow!(
        "No graph matches '{query}'. Available graphs:\n{}",
        candidate_list(all.iter())
    )
}

pub(super) fn ambiguous_error(query: &str, matches: &[&Graph]) -> anyhow::Error {
    anyhow!(
        "'{query}' matches multiple graphs:\n{}\nUse the full id to disambiguate.",
        candidate_list(matches.iter().copied())
    )
}

pub(super) fn candidate_list<'a>(graphs: impl Iterator<Item = &'a Graph>) -> String {
    graphs
        .map(|l| format!("  {} ({})", l.name, short_id(&l.id)))
        .collect::<Vec<_>>()
        .join("\n")
}
