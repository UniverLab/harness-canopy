//! CM50: `canopy graph crew` — the read-only CLI over the `graph_crew`
//! surface. Same code path as the MCP tool (both call
//! [`Database::graph_crew`] / [`Database::graph_crew_summary`]): one table
//! per graph, `--json` for the MCP shape, `--group-by-pair` for the
//! platform/model summary.

use anyhow::Result;

use super::format::{format_graph_crew_summary_table, format_graph_crew_table, short_id};
use super::resolve::resolve_graph;
use crate::db::graph_crew::GraphCrewDbParams;
use crate::db::Database;
use crate::domain::graphs::{GraphCrewGroup, GraphCrewSummary};

/// Print one graph's (or every graph's) harness/model assignments.
///
/// `id_or_name` resolves through the same id/name/prefix rule every
/// `canopy graph` subcommand uses, so an archived graph is still reachable
/// by name even while `--all` is off.
pub(super) fn handle_graph_crew(
    db: &Database,
    id_or_name: Option<&str>,
    platform: Option<&str>,
    model: Option<&str>,
    all: bool,
    group_by_pair: bool,
    json: bool,
) -> Result<()> {
    let graph_id = match id_or_name {
        Some(query) => {
            let graphs = db.list_graphs(None, true)?;
            Some(resolve_graph(&graphs, query)?.id.clone())
        }
        None => None,
    };
    let params = GraphCrewDbParams {
        graph_id: graph_id.as_deref(),
        platform,
        model,
        include_archived: all,
    };

    if group_by_pair {
        let summaries = db.graph_crew_summary(params)?;
        print_summary(&summaries, json)?;
        return Ok(());
    }

    let groups = db.graph_crew(params)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&groups)?);
        return Ok(());
    }
    print_crew_tables(&groups);
    Ok(())
}

/// One table per graph, headed by the graph's name, short id and status.
fn print_crew_tables(groups: &[GraphCrewGroup]) {
    if groups.is_empty() {
        println!("No matching crew assignments found.");
        return;
    }
    for group in groups {
        println!(
            "\n\x1b[1m── {} ──\x1b[0m  \x1b[90m{} {} {}\x1b[0m",
            group.name,
            short_id(&group.id),
            group.status,
            group.workdir,
        );
        println!("{}", format_graph_crew_table(&group.rows));
    }
    println!();
}

/// `--group-by-pair`: the summary table, or the MCP summary shape as JSON.
fn print_summary(summaries: &[GraphCrewSummary], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summaries)?);
        return Ok(());
    }
    println!("{}", format_graph_crew_summary_table(summaries));
    println!();
    Ok(())
}
