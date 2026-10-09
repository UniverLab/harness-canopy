//! CLI handlers for `canopy graph` subcommands.
//!
//! `list`/`info`/`export` are read-only: mirror `canopy rag report` (see
//! `rag_cli.rs`) in spirit — a terminal view onto state that previously
//! required querying the database directly. Every other
//! subcommand (`import`/`run`/`pause`/`continue`/`reset`/`autorun`) changes
//! graph state, so it resolves the target graph (or, for `import`, has
//! nothing yet to resolve) against that same local database (exactly as
//! `list`/`info` already do — see [`resolve_graph`]) but then delegates the
//! actual mutation to the daemon's MCP tool of the same name via
//! `daemon::cli_daemon::call_tool`, never touching the database itself.
//! This is the second surface for the operations `graph_import`/`graph_run`/
//! `graph_pause`/`graph_continue`/`graph_reset`/`graph_schedule_autorun` already
//! expose over MCP — for when an MCP client can't reach them but the daemon
//! and its database still can.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::domain::graphs::GraphStatus;

use format::{current_spec_name, format_autorun_compact, graph_progress, short_id, status_icon};
use info::handle_graph_info;
use resolve::resolve_graph;

#[cfg(test)]
use crate::daemon::handler::{active_graph_spec_context, ActiveGraphSpec};
#[cfg(test)]
use crate::domain::graphs::{Graph, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus};
#[cfg(test)]
use chrono::{DateTime, Utc};
#[cfg(test)]
use format::{
    commit_rights_note, current_running_run, current_spec, distinct_spec_ids_in_order, format_dt,
    format_elapsed, format_relative_duration, interrupted_note, is_interrupted, no_verdict_note,
    run_status_icon, spec_progress, spec_status_icon,
};
#[cfg(test)]
use info::{
    print_graph_info_current_node, print_graph_info_hook_runs, print_graph_info_recent_runs,
};
#[cfg(test)]
use resolve::{ambiguous_error, candidate_list, not_found_error};

mod format;
mod info;
mod resolve;

#[derive(Subcommand, Debug)]
pub(crate) enum GraphAction {
    /// List all graphs with status and spec progress.
    List {
        /// Only show graphs tagged with this workdir.
        #[arg(long)]
        workdir: Option<String>,
    },
    /// Show detailed status for a single graph.
    Info {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
    },
    /// Export a graph's design (name, description, nodes, edges, ensembles)
    /// as a portable JSON document, so it can be shared as a file and
    /// recreated elsewhere with `import`.
    Export {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// Write the exported JSON to this path instead of stdout.
        #[arg(long)]
        output: Option<String>,
    },
    /// Create a new graph from an exported document — never overwrites an
    /// existing graph.
    Import {
        /// Path to an exported graph JSON file.
        path: String,
        /// Absolute workdir for the new graph. Defaults to the current
        /// directory.
        #[arg(long)]
        workdir: Option<String>,
        /// Graph name to use instead of the file's own name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Run a graph in the background, spec by spec.
    Run {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// Run this queue's pending specs (in queue order) through the
        /// graph instead of the graph's own bound specs.
        #[arg(long)]
        queue: Option<String>,
        /// Absolute workdir override for this run only.
        #[arg(long)]
        workdir: Option<String>,
        /// Free-form text fed to nodes as `{{spec_content}}` when the graph has
        /// no bound specs and no queue.
        #[arg(long)]
        idea: Option<String>,
        /// Start the first spec this run executes at this node (id, id prefix,
        /// node name, or ensemble name) instead of Start.
        #[arg(long = "from")]
        from: Option<String>,
    },
    /// Pause a running graph after the current node finishes.
    Pause {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
    },
    /// Continue a paused graph by retrying the current node or skipping to
    /// the next spec.
    Continue {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// Retry the node that was running when the graph paused.
        #[arg(long = "retry-current-node")]
        retry_current_node: bool,
        /// Skip the current spec and move on to the next one.
        #[arg(long = "skip-next-spec")]
        skip_next_spec: bool,
        /// Start the current (or --spec) spec at this node instead of resuming
        /// the paused node: id, id prefix, node name, or ensemble name.
        #[arg(long = "from")]
        from: Option<String>,
        /// Spec to start at when using --from. Defaults to the graph's current spec.
        #[arg(long)]
        spec: Option<String>,
    },
    /// Reset a completed/failed graph back to pending so `run` can relaunch
    /// it.
    Reset {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// Reset exactly these spec IDs, even if already completed. Omit to
        /// reset every non-completed spec, leaving completed ones untouched.
        #[arg(long)]
        specs: Vec<String>,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Schedule a one-shot future resume for a graph, or cancel a pending one.
    Autorun {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// ISO 8601 timestamp at which the graph should resume, e.g.
        /// "2026-07-10T09:00:00Z".
        #[arg(long)]
        at: Option<String>,
        /// Raw CLI quota-limit message (e.g. "resets 1pm (America/Bogota)")
        /// to compute the resume instant from, instead of passing `--at`.
        #[arg(long = "quota-reset-message")]
        quota_reset_message: Option<String>,
        /// Cancel any pending autorun instead of scheduling one.
        #[arg(long)]
        cancel: bool,
    },
    /// Unbind a spec from a graph, making it a standalone backlog spec.
    RemoveSpec {
        /// Full graph id, an unambiguous id prefix, or the exact graph name.
        id_or_name: String,
        /// Spec ID (or unambiguous prefix) to unbind.
        spec_id: String,
    },
}

pub(crate) async fn handle_graph_action(
    action: GraphAction,
    port_override: Option<u16>,
) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = Database::new_safe(&database_path(&data_dir), &data_dir)?;

    match action {
        GraphAction::List { workdir } => handle_graph_list(&db, workdir.as_deref()),
        GraphAction::Info { id_or_name } => handle_graph_info(&db, &id_or_name),
        GraphAction::Export { id_or_name, output } => {
            handle_graph_export(&db, &id_or_name, output.as_deref())
        }
        GraphAction::Import {
            path,
            workdir,
            name,
        } => handle_graph_import(port_override, &path, workdir, name),
        GraphAction::Run {
            id_or_name,
            queue,
            workdir,
            idea,
            from,
        } => handle_graph_run(&db, port_override, &id_or_name, queue, workdir, idea, from),
        GraphAction::Pause { id_or_name } => handle_graph_pause(&db, port_override, &id_or_name),
        GraphAction::Continue {
            id_or_name,
            retry_current_node,
            skip_next_spec,
            from,
            spec,
        } => handle_graph_continue(
            &db,
            port_override,
            &id_or_name,
            retry_current_node,
            skip_next_spec,
            from,
            spec,
        ),
        GraphAction::Reset {
            id_or_name,
            specs,
            yes,
        } => handle_graph_reset(&db, port_override, &id_or_name, &specs, yes),
        GraphAction::Autorun {
            id_or_name,
            at,
            quota_reset_message,
            cancel,
        } => handle_graph_autorun(
            &db,
            port_override,
            &id_or_name,
            at.as_deref(),
            quota_reset_message.as_deref(),
            cancel,
        ),
        GraphAction::RemoveSpec {
            id_or_name,
            spec_id,
        } => handle_graph_remove_spec(&db, port_override, &id_or_name, &spec_id),
    }
}

fn handle_graph_run(
    db: &Database,
    port_override: Option<u16>,
    id_or_name: &str,
    queue: Option<String>,
    workdir: Option<String>,
    idea: Option<String>,
    from: Option<String>,
) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    let mut args = serde_json::json!({ "graph_id": lp.id });
    if let Some(queue_id) = queue {
        args["queue_id"] = serde_json::json!(queue_id);
    }
    if let Some(workdir) = workdir {
        args["workdir"] = serde_json::json!(workdir);
    }
    if let Some(idea_text) = idea {
        args["idea"] = serde_json::json!(idea_text);
    }
    if let Some(node) = from {
        args["start_node_id"] = serde_json::json!(node);
    }

    println!("{}", call_tool(port_override, "graph_run", &args)?);
    Ok(())
}

fn handle_graph_pause(db: &Database, port_override: Option<u16>, id_or_name: &str) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    println!(
        "{}",
        call_tool(
            port_override,
            "graph_pause",
            &serde_json::json!({ "graph_id": lp.id }),
        )?
    );
    Ok(())
}

fn handle_graph_remove_spec(
    db: &Database,
    port_override: Option<u16>,
    id_or_name: &str,
    spec_id: &str,
) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;
    println!(
        "{}",
        call_tool(
            port_override,
            "graph_remove_spec",
            &serde_json::json!({
                "graph_id": lp.id,
                "spec_id": spec_id,
            }),
        )?
    );
    Ok(())
}

fn handle_graph_continue(
    db: &Database,
    port_override: Option<u16>,
    id_or_name: &str,
    retry_current_node: bool,
    skip_next_spec: bool,
    from: Option<String>,
    spec: Option<String>,
) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    if let Some(node) = from {
        if retry_current_node || skip_next_spec {
            return Err(anyhow!(
                "--from cannot be combined with --retry-current-node or --skip-next-spec."
            ));
        }
        let mut args = serde_json::json!({
            "graph_id": lp.id, "action": "run_from_node", "node_id": node
        });
        if let Some(spec) = spec {
            args["spec_id"] = serde_json::json!(spec);
        }
        println!("{}", call_tool(port_override, "graph_continue", &args)?);
        return Ok(());
    }

    let action = match (retry_current_node, skip_next_spec) {
        (true, false) => "retry_current_node",
        (false, true) => "skip_next_spec",
        (false, false) => {
            return Err(anyhow!(
                "Specify one of --retry-current-node or --skip-next-spec."
            ))
        }
        (true, true) => {
            return Err(anyhow!(
                "--retry-current-node and --skip-next-spec are mutually exclusive."
            ))
        }
    };

    println!(
        "{}",
        call_tool(
            port_override,
            "graph_continue",
            &serde_json::json!({ "graph_id": lp.id, "action": action }),
        )?
    );
    Ok(())
}

fn handle_graph_reset(
    db: &Database,
    port_override: Option<u16>,
    id_or_name: &str,
    specs: &[String],
    yes: bool,
) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    if !yes && !confirm_reset(&lp.name)? {
        println!("Aborted.");
        return Ok(());
    }

    let mut args = serde_json::json!({ "graph_id": lp.id });
    if !specs.is_empty() {
        args["specs"] = serde_json::json!(specs);
    }

    println!("{}", call_tool(port_override, "graph_reset", &args)?);
    Ok(())
}

/// Interactive confirmation for `graph reset`, bypassable with `--yes` — the
/// same `inquire::Confirm`-with-default-false pattern `canopy clean --hard`
/// already uses for its own destructive cascade, and the CLI counterpart to
/// the TUI's y/n modal for a graph deletion.
fn confirm_reset(graph_name: &str) -> Result<bool> {
    use inquire::Confirm;
    Confirm::new(&format!(
        "Reset graph '{graph_name}' back to pending? This clears progress on its non-completed specs."
    ))
    .with_default(false)
    .with_help_message("y: reset, n/Esc: abort")
    .prompt()
    .map_err(|err| anyhow!("{err}"))
}

/// Read-only, like `list`/`info` — reads the local database directly rather
/// than round-tripping through the daemon, so exporting a graph never
/// requires the daemon to be running.
fn handle_graph_export(db: &Database, id_or_name: &str, output: Option<&str>) -> Result<()> {
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    let graph_nodes = db.list_graph_nodes_for_graph(&lp.id)?;
    let graph_edges = db.list_graph_edges_for_graph(&lp.id)?;
    let ensembles = db.list_ensembles_for_graph(&lp.id)?;
    let document = crate::domain::graph_transfer::build_export_document(
        lp,
        &graph_nodes,
        &graph_edges,
        &ensembles,
    )
    .map_err(|e| anyhow!(e))?;
    let json = serde_json::to_string_pretty(&document)?;

    match output {
        Some(path) => {
            std::fs::write(path, format!("{json}\n"))
                .with_context(|| format!("could not write '{path}'"))?;
            println!("Exported graph '{}' to {path}.", lp.name);
        }
        None => println!("{json}"),
    }
    Ok(())
}

/// Import creates a new graph, which needs the daemon's own side effects
/// (project path registration, trigger activation) — so unlike `export`
/// this delegates to the daemon's `graph_import` MCP tool rather than
/// writing to the database directly (see module doc: every mutation goes
/// through the daemon).
fn handle_graph_import(
    port_override: Option<u16>,
    path: &str,
    workdir: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("could not read '{path}'"))?;
    // Validated locally first so a malformed file (bad JSON, missing
    // format_version) fails fast with a clear message instead of a round
    // trip to the daemon — `graph_import` re-validates it anyway (decision
    // 5), so this is a UX nicety, not the source of truth.
    let document = crate::domain::graph_transfer::parse_export_document_str(&raw)
        .map_err(|e| anyhow!("'{path}': {e}"))?;
    let document = serde_json::to_value(&document)?;

    let workdir = match workdir {
        Some(workdir) => workdir,
        None => std::env::current_dir()
            .context("could not determine the current directory")?
            .to_string_lossy()
            .to_string(),
    };

    let mut args = serde_json::json!({ "document": document, "workdir": workdir });
    if let Some(name) = name {
        args["name"] = serde_json::json!(name);
    }

    println!("{}", call_tool(port_override, "graph_import", &args)?);
    Ok(())
}

fn handle_graph_autorun(
    db: &Database,
    port_override: Option<u16>,
    id_or_name: &str,
    at: Option<&str>,
    quota_reset_message: Option<&str>,
    cancel: bool,
) -> Result<()> {
    let set_count = [at.is_some(), quota_reset_message.is_some(), cancel]
        .iter()
        .filter(|set| **set)
        .count();
    if set_count == 0 {
        return Err(anyhow!(
            "Specify one of --at, --quota-reset-message, or --cancel."
        ));
    }
    if set_count > 1 {
        return Err(anyhow!(
            "--at, --quota-reset-message, and --cancel are mutually exclusive."
        ));
    }

    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    let args = if cancel {
        serde_json::json!({ "graph_id": lp.id })
    } else if let Some(at) = at {
        serde_json::json!({ "graph_id": lp.id, "at": at })
    } else {
        serde_json::json!({ "graph_id": lp.id, "quota_reset_message": quota_reset_message })
    };

    println!(
        "{}",
        call_tool(port_override, "graph_schedule_autorun", &args)?
    );
    Ok(())
}

fn handle_graph_list(db: &Database, workdir: Option<&str>) -> Result<()> {
    let graphs = db.list_graphs(workdir, false)?;

    if graphs.is_empty() {
        println!("No graphs found.");
        return Ok(());
    }

    println!("\n\x1b[1m── Canopy Graphs ───────────────────────────────────────────────\x1b[0m\n");
    for lp in &graphs {
        let specs = db.list_graph_specs(&lp.id)?;
        let (done, total) = graph_progress(db, lp, &specs)?;

        let mut line = format!(
            " {} {}  \x1b[90m{}\x1b[0m  {:<9} {done}/{total}",
            status_icon(lp.status),
            lp.name,
            short_id(&lp.id),
            lp.status.as_str(),
        );

        if lp.status == GraphStatus::Running {
            if let Some(name) = current_spec_name(db, lp, &specs)? {
                line.push_str(&format!("  → {name}"));
            }
        }

        if let Some(at) = lp.autorun_at {
            line.push_str(&format!("  \x1b[36m{}\x1b[0m", format_autorun_compact(at)));
        }

        println!("{line}");
    }
    println!();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;

    /// Redirects fd 1 into a pipe for the lifetime of the guard, so a
    /// section printer that writes straight to stdout can be asserted on.
    /// `cargo nextest` runs one test per process, so the process-wide fd
    /// swap is isolated; this crate's protocol mandates nextest.
    struct StdoutCapture {
        saved_fd: i32,
        read_end: std::fs::File,
    }

    impl StdoutCapture {
        fn start() -> Self {
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe() failed");
            let (read_fd, write_fd) = (fds[0], fds[1]);
            let saved_fd = unsafe { libc::dup(1) };
            assert!(saved_fd >= 0, "dup(1) failed");
            std::io::stdout().flush().unwrap();
            let rc = unsafe { libc::dup2(write_fd, 1) };
            assert_eq!(rc, 1, "dup2 failed");
            unsafe { libc::close(write_fd) };
            StdoutCapture {
                saved_fd,
                read_end: unsafe { std::fs::File::from_raw_fd(read_fd) },
            }
        }

        fn stop(mut self) -> String {
            std::io::stdout().flush().unwrap();
            unsafe {
                libc::dup2(self.saved_fd, 1);
                libc::close(self.saved_fd);
            }
            let mut buf = String::new();
            self.read_end.read_to_string(&mut buf).unwrap();
            buf
        }
    }

    /// Byte index of `needle`'s first occurrence, for asserting section
    /// order without depending on the exact surrounding bytes.
    fn assert_ordered(haystack: &str, needles: &[&str]) {
        let mut cursor = 0usize;
        for needle in needles {
            let at = haystack[cursor..]
                .find(needle)
                .unwrap_or_else(|| panic!("missing {needle:?} after byte {cursor}"));
            cursor += at + needle.len();
        }
    }

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: GraphAction,
    }

    #[test]
    fn list_parses_with_and_without_workdir() {
        let cli = TestCli::try_parse_from(["test", "list"]).expect("list should parse");
        assert!(matches!(cli.action, GraphAction::List { workdir: None }));

        let cli = TestCli::try_parse_from(["test", "list", "--workdir", "/tmp/proj"])
            .expect("list --workdir should parse");
        match cli.action {
            GraphAction::List { workdir } => assert_eq!(workdir.as_deref(), Some("/tmp/proj")),
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn info_requires_id_or_name() {
        assert!(TestCli::try_parse_from(["test", "info"]).is_err());
        let cli = TestCli::try_parse_from(["test", "info", "my-graph"]).expect("should parse");
        match cli.action {
            GraphAction::Info { id_or_name } => assert_eq!(id_or_name, "my-graph"),
            other => panic!("expected Info, got {other:?}"),
        }
    }

    #[test]
    fn run_parses_queue_and_workdir() {
        let cli = TestCli::try_parse_from(["test", "run", "my-graph"]).expect("should parse");
        match cli.action {
            GraphAction::Run {
                id_or_name,
                queue,
                workdir,
                idea,
                from,
            } => {
                assert_eq!(id_or_name, "my-graph");
                assert!(queue.is_none());
                assert!(workdir.is_none());
                assert!(idea.is_none());
                assert!(from.is_none());
            }
            other => panic!("expected Run, got {other:?}"),
        }

        let cli = TestCli::try_parse_from([
            "test",
            "run",
            "my-graph",
            "--queue",
            "q1",
            "--workdir",
            "/tmp/proj",
        ])
        .expect("should parse");
        match cli.action {
            GraphAction::Run {
                id_or_name,
                queue,
                workdir,
                idea,
                from,
            } => {
                assert_eq!(id_or_name, "my-graph");
                assert_eq!(queue.as_deref(), Some("q1"));
                assert_eq!(workdir.as_deref(), Some("/tmp/proj"));
                assert!(idea.is_none());
                assert!(from.is_none());
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn pause_requires_id_or_name() {
        assert!(TestCli::try_parse_from(["test", "pause"]).is_err());
        let cli = TestCli::try_parse_from(["test", "pause", "my-graph"]).expect("should parse");
        match cli.action {
            GraphAction::Pause { id_or_name } => assert_eq!(id_or_name, "my-graph"),
            other => panic!("expected Pause, got {other:?}"),
        }
    }

    #[test]
    fn export_parses_output() {
        let cli = TestCli::try_parse_from(["test", "export", "my-graph"]).expect("should parse");
        match cli.action {
            GraphAction::Export { id_or_name, output } => {
                assert_eq!(id_or_name, "my-graph");
                assert!(output.is_none());
            }
            other => panic!("expected Export, got {other:?}"),
        }

        let cli = TestCli::try_parse_from(["test", "export", "my-graph", "--output", "graph.json"])
            .expect("should parse");
        match cli.action {
            GraphAction::Export { id_or_name, output } => {
                assert_eq!(id_or_name, "my-graph");
                assert_eq!(output.as_deref(), Some("graph.json"));
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn import_parses_workdir_and_name() {
        let cli = TestCli::try_parse_from(["test", "import", "graph.json"]).expect("should parse");
        match cli.action {
            GraphAction::Import {
                path,
                workdir,
                name,
            } => {
                assert_eq!(path, "graph.json");
                assert!(workdir.is_none());
                assert!(name.is_none());
            }
            other => panic!("expected Import, got {other:?}"),
        }

        let cli = TestCli::try_parse_from([
            "test",
            "import",
            "graph.json",
            "--workdir",
            "/tmp/proj",
            "--name",
            "New Name",
        ])
        .expect("should parse");
        match cli.action {
            GraphAction::Import {
                path,
                workdir,
                name,
            } => {
                assert_eq!(path, "graph.json");
                assert_eq!(workdir.as_deref(), Some("/tmp/proj"));
                assert_eq!(name.as_deref(), Some("New Name"));
            }
            other => panic!("expected Import, got {other:?}"),
        }
    }

    #[test]
    fn continue_parses_retry_and_skip_flags() {
        let cli = TestCli::try_parse_from(["test", "continue", "my-graph", "--retry-current-node"])
            .expect("should parse");
        match cli.action {
            GraphAction::Continue {
                id_or_name,
                retry_current_node,
                skip_next_spec,
                from,
                spec,
            } => {
                assert_eq!(id_or_name, "my-graph");
                assert!(retry_current_node);
                assert!(!skip_next_spec);
                assert!(from.is_none());
                assert!(spec.is_none());
            }
            other => panic!("expected Continue, got {other:?}"),
        }

        let cli = TestCli::try_parse_from(["test", "continue", "my-graph", "--skip-next-spec"])
            .expect("should parse");
        match cli.action {
            GraphAction::Continue { skip_next_spec, .. } => assert!(skip_next_spec),
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    #[test]
    fn reset_parses_repeated_specs_and_yes() {
        let cli = TestCli::try_parse_from([
            "test", "reset", "my-graph", "--specs", "a", "--specs", "b", "--yes",
        ])
        .expect("should parse");
        match cli.action {
            GraphAction::Reset {
                id_or_name,
                specs,
                yes,
            } => {
                assert_eq!(id_or_name, "my-graph");
                assert_eq!(specs, vec!["a".to_string(), "b".to_string()]);
                assert!(yes);
            }
            other => panic!("expected Reset, got {other:?}"),
        }
    }

    #[test]
    fn autorun_parses_at_quota_message_and_cancel() {
        let cli = TestCli::try_parse_from([
            "test",
            "autorun",
            "my-graph",
            "--at",
            "2026-07-10T09:00:00Z",
        ])
        .expect("should parse");
        match cli.action {
            GraphAction::Autorun { at, .. } => {
                assert_eq!(at.as_deref(), Some("2026-07-10T09:00:00Z"));
            }
            other => panic!("expected Autorun, got {other:?}"),
        }

        let cli = TestCli::try_parse_from(["test", "autorun", "my-graph", "--cancel"])
            .expect("should parse");
        match cli.action {
            GraphAction::Autorun { cancel, .. } => assert!(cancel),
            other => panic!("expected Autorun, got {other:?}"),
        }
    }

    fn make_graph(id: &str, name: &str, status: GraphStatus) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    fn make_spec(graph_id: &str, name: &str, position: i64, status: GraphSpecStatus) -> GraphSpec {
        GraphSpec {
            id: format!("{graph_id}-{position}"),
            graph_id: Some(graph_id.to_string()),
            name: name.to_string(),
            description: None,
            position,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    #[test]
    fn spec_progress_counts_only_completed_as_done() {
        let specs = vec![
            make_spec("l1", "a", 0, GraphSpecStatus::Completed),
            make_spec("l1", "b", 1, GraphSpecStatus::Running),
            make_spec("l1", "c", 2, GraphSpecStatus::Pending),
            make_spec("l1", "d", 3, GraphSpecStatus::Skipped),
        ];
        assert_eq!(spec_progress(&specs), (1, 4));
    }

    #[test]
    fn spec_progress_empty_is_zero_of_zero() {
        assert_eq!(spec_progress(&[]), (0, 0));
    }

    #[test]
    fn graph_progress_counts_queue_members_when_active_run_queue_id_set() {
        use crate::domain::queues::Queue;

        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();

        // A queue-driven graph binds no specs of its own, so counting bound
        // specs would render a misleading 0/0.
        let mut lp = make_graph("graph-1", "queue-graph", GraphStatus::Running);
        lp.active_run_queue_id = Some("queue-1".to_string());
        db.insert_graph(&lp).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "P".to_string(),
            created_at: Utc::now(),
        })
        .unwrap();

        // Two standalone queue members (graph_id: None, like real ones): one
        // completed, one pending.
        for (id, status) in [
            ("spec-a", GraphSpecStatus::Completed),
            ("spec-b", GraphSpecStatus::Pending),
        ] {
            let mut spec = make_spec("queue", id, 0, status);
            spec.id = id.to_string();
            spec.graph_id = None;
            db.insert_graph_spec(&spec).unwrap();
            db.append_queue_member("queue-1", id, None).unwrap();
        }

        // Bound specs empty; queue progress is 1/2.
        assert_eq!(graph_progress(&db, &lp, &[]).unwrap(), (1, 2));
    }

    #[test]
    fn graph_progress_shows_n_of_n_for_completed_queue_graph() {
        use crate::domain::queues::Queue;

        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();

        // B31: a finished queue-driven graph keeps its `active_run_queue_id`, so
        // even in a terminal status it must still render its real n/n queue
        // progress rather than the 0/0 a bound-spec count would produce.
        let mut lp = make_graph("graph-done", "queue-graph", GraphStatus::Completed);
        lp.active_run_queue_id = Some("queue-1".to_string());
        db.insert_graph(&lp).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "P".to_string(),
            created_at: Utc::now(),
        })
        .unwrap();

        for id in ["spec-a", "spec-b"] {
            let mut spec = make_spec("queue", id, 0, GraphSpecStatus::Completed);
            spec.id = id.to_string();
            spec.graph_id = None;
            db.insert_graph_spec(&spec).unwrap();
            db.append_queue_member("queue-1", id, None).unwrap();
        }

        assert_eq!(graph_progress(&db, &lp, &[]).unwrap(), (2, 2));
    }

    #[test]
    fn graph_progress_falls_back_to_bound_specs_without_queue() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        let lp = make_graph("graph-2", "bound", GraphStatus::Running);
        let specs = vec![
            make_spec("graph-2", "a", 0, GraphSpecStatus::Completed),
            make_spec("graph-2", "b", 1, GraphSpecStatus::Pending),
        ];
        assert_eq!(graph_progress(&db, &lp, &specs).unwrap(), (1, 2));
    }

    #[test]
    fn current_spec_prefers_running_over_pending() {
        let specs = vec![
            make_spec("l1", "a", 0, GraphSpecStatus::Completed),
            make_spec("l1", "b", 1, GraphSpecStatus::Running),
            make_spec("l1", "c", 2, GraphSpecStatus::Pending),
        ];
        assert_eq!(current_spec(&specs).map(|s| s.name.as_str()), Some("b"));
    }

    #[test]
    fn current_spec_falls_back_to_next_pending() {
        let specs = vec![
            make_spec("l1", "a", 0, GraphSpecStatus::Completed),
            make_spec("l1", "c", 2, GraphSpecStatus::Pending),
        ];
        assert_eq!(current_spec(&specs).map(|s| s.name.as_str()), Some("c"));
    }

    #[test]
    fn current_spec_none_when_all_terminal() {
        let specs = vec![
            make_spec("l1", "a", 0, GraphSpecStatus::Completed),
            make_spec("l1", "b", 1, GraphSpecStatus::Failed),
        ];
        assert!(current_spec(&specs).is_none());
    }

    fn make_run(
        spec_id: &str,
        node_id: &str,
        status: GraphRunStatus,
        started_at_secs: i64,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: format!("{spec_id}-{node_id}-{started_at_secs}"),
            graph_id: "l1".to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status,
            input: None,
            output: None,
            started_at: DateTime::<Utc>::from_timestamp(started_at_secs, 0).unwrap(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    #[test]
    fn current_running_run_picks_the_in_flight_one() {
        let runs = vec![
            make_run("s1", "n1", GraphRunStatus::Pass, 1),
            make_run("s2", "n2", GraphRunStatus::Running, 2),
        ];
        assert_eq!(
            current_running_run(&runs).map(|r| r.node_id.as_str()),
            Some("n2")
        );
    }

    #[test]
    fn current_running_run_none_when_all_terminal() {
        let runs = vec![
            make_run("s1", "n1", GraphRunStatus::Pass, 1),
            make_run("s2", "n2", GraphRunStatus::Fail, 2),
        ];
        assert!(current_running_run(&runs).is_none());
    }

    #[test]
    fn distinct_spec_ids_in_order_dedupes_preserving_first_seen() {
        let runs = vec![
            make_run("s1", "n1", GraphRunStatus::Pass, 1),
            make_run("s2", "n1", GraphRunStatus::Pass, 2),
            make_run("s1", "n2", GraphRunStatus::Pass, 3),
        ];
        assert_eq!(distinct_spec_ids_in_order(&runs), vec!["s1", "s2"]);
    }

    #[test]
    fn distinct_spec_ids_in_order_empty() {
        assert!(distinct_spec_ids_in_order(&[]).is_empty());
    }

    #[test]
    fn resolve_graph_by_exact_id() {
        let graphs = vec![
            make_graph("abc123", "one", GraphStatus::Draft),
            make_graph("def456", "two", GraphStatus::Draft),
        ];
        let resolved = resolve_graph(&graphs, "def456").unwrap();
        assert_eq!(resolved.name, "two");
    }

    #[test]
    fn resolve_graph_by_exact_name() {
        let graphs = vec![
            make_graph("abc123", "one", GraphStatus::Draft),
            make_graph("def456", "two", GraphStatus::Draft),
        ];
        let resolved = resolve_graph(&graphs, "two").unwrap();
        assert_eq!(resolved.id, "def456");
    }

    #[test]
    fn resolve_graph_by_unambiguous_prefix() {
        let graphs = vec![
            make_graph("abc123", "one", GraphStatus::Draft),
            make_graph("def456", "two", GraphStatus::Draft),
        ];
        let resolved = resolve_graph(&graphs, "abc").unwrap();
        assert_eq!(resolved.name, "one");
    }

    #[test]
    fn resolve_graph_ambiguous_prefix_lists_candidates() {
        let graphs = vec![
            make_graph("abc123", "one", GraphStatus::Draft),
            make_graph("abc789", "two", GraphStatus::Draft),
        ];
        let err = resolve_graph(&graphs, "abc").unwrap_err().to_string();
        assert!(err.contains("multiple graphs"));
        assert!(err.contains("one"));
        assert!(err.contains("two"));
    }

    #[test]
    fn resolve_graph_ambiguous_name_lists_candidates() {
        let graphs = vec![
            make_graph("abc123", "dup", GraphStatus::Draft),
            make_graph("def456", "dup", GraphStatus::Draft),
        ];
        let err = resolve_graph(&graphs, "dup").unwrap_err().to_string();
        assert!(err.contains("multiple graphs"));
        assert!(err.contains("abc123"));
        assert!(err.contains("def456"));
    }

    #[test]
    fn resolve_graph_not_found_lists_all_candidates() {
        let graphs = vec![make_graph("abc123", "one", GraphStatus::Draft)];
        let err = resolve_graph(&graphs, "missing").unwrap_err().to_string();
        assert!(err.contains("No graph matches 'missing'"));
        assert!(err.contains("one"));
    }

    #[test]
    fn resolve_graph_not_found_on_empty_set() {
        let err = resolve_graph(&[], "anything").unwrap_err().to_string();
        assert!(err.contains("no graphs exist yet"));
    }

    #[test]
    fn resolve_graph_rejects_empty_query() {
        let graphs = vec![make_graph("abc123", "one", GraphStatus::Draft)];
        assert!(resolve_graph(&graphs, "").is_err());
        assert!(resolve_graph(&graphs, "   ").is_err());
    }

    #[test]
    fn status_icons_are_distinct_per_status() {
        let statuses = [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ];
        let icons: std::collections::HashSet<&str> =
            statuses.iter().map(|s| status_icon(*s)).collect();
        assert_eq!(icons.len(), statuses.len());
    }

    #[test]
    fn format_elapsed_buckets() {
        assert_eq!(format_elapsed(chrono::Duration::seconds(5)), "5s");
        assert_eq!(format_elapsed(chrono::Duration::seconds(65)), "1m5s");
        assert_eq!(format_elapsed(chrono::Duration::seconds(3661)), "1h1m");
    }

    #[test]
    fn format_relative_duration_buckets() {
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(30)),
            "in 30s"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::minutes(1)),
            "in 1m"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::minutes(134)),
            "in 2h 14m"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::hours(3)),
            "in 3h"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::hours(30)),
            "in 1d"
        );
    }

    #[test]
    fn format_relative_duration_past_or_now_reads_due_now() {
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(0)),
            "due now"
        );
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(-5)),
            "due now"
        );
    }

    #[test]
    fn format_autorun_compact_renders_utc_badge() {
        let at = DateTime::<Utc>::from_timestamp(5 * 3600, 0).unwrap(); // 05:00:00 UTC
        assert_eq!(format_autorun_compact(at), "⏲ 05:00Z");
    }

    #[test]
    fn short_id_truncates_to_eight_chars() {
        assert_eq!(short_id("abcdef1234567890"), "abcdef12");
        assert_eq!(short_id("short"), "short");
        assert_eq!(short_id(""), "");
    }

    #[test]
    fn spec_status_icons_are_distinct_per_status() {
        let statuses = [
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Pending,
            GraphSpecStatus::Interrupted,
        ];
        let icons: std::collections::HashSet<&str> =
            statuses.iter().map(|s| spec_status_icon(*s)).collect();
        assert_eq!(icons.len(), statuses.len());
    }

    #[test]
    fn run_status_icon_shows_flag_for_interrupted_runs() {
        let output = Some(serde_json::json!({"interrupted": true}));
        assert!(run_status_icon(GraphRunStatus::Fail, output.as_ref()).contains("⚑"));
        assert!(run_status_icon(GraphRunStatus::Pass, output.as_ref()).contains("⚑"));
        assert!(run_status_icon(GraphRunStatus::Running, output.as_ref()).contains("⚑"));
    }

    #[test]
    fn run_status_icon_shows_normal_icons_when_not_interrupted() {
        let output = Some(serde_json::json!({"interrupted": false}));
        assert!(run_status_icon(GraphRunStatus::Pass, output.as_ref()).contains("✓"));
        assert!(run_status_icon(GraphRunStatus::Fail, output.as_ref()).contains("✗"));
        assert!(run_status_icon(GraphRunStatus::Running, output.as_ref()).contains("▶"));
    }

    #[test]
    fn run_status_icon_handles_none_output() {
        assert!(run_status_icon(GraphRunStatus::Pass, None).contains("✓"));
        assert!(run_status_icon(GraphRunStatus::Fail, None).contains("✗"));
        assert!(run_status_icon(GraphRunStatus::Running, None).contains("▶"));
    }

    #[test]
    fn run_status_icon_error_is_warning_not_fail() {
        let icon = run_status_icon(GraphRunStatus::Error, None);
        assert!(
            icon.contains("⚠"),
            "Error must render a warning icon, got {icon:?}"
        );
        assert!(!icon.contains("✗"), "Error must not render the fail icon");
    }

    #[test]
    fn no_verdict_note_renders_only_for_no_verdict() {
        let out = serde_json::json!({"no_verdict": true});
        assert!(
            no_verdict_note(Some(&out)).contains("error (no verdict)"),
            "no_verdict output must render the note"
        );
        let plain = serde_json::json!({"kind": "quorum"});
        assert_eq!(no_verdict_note(Some(&plain)), "");
        assert_eq!(no_verdict_note(None), "");
    }

    #[test]
    fn is_interrupted_detects_flag() {
        assert!(is_interrupted(Some(
            &serde_json::json!({"interrupted": true})
        )));
        assert!(!is_interrupted(Some(
            &serde_json::json!({"interrupted": false})
        )));
        assert!(!is_interrupted(Some(
            &serde_json::json!({"other": "field"})
        )));
        assert!(!is_interrupted(None));
    }

    #[test]
    fn commit_rights_note_renders_violation() {
        let output = Some(serde_json::json!({
            "commit_rights_violation": {"head_after": "abcdef1234567890"}
        }));
        let note = commit_rights_note(output.as_ref());
        assert!(note.contains("no commit rights"));
        assert!(note.contains("abcdef12"));
    }

    #[test]
    fn commit_rights_note_empty_when_no_violation() {
        assert!(commit_rights_note(None).is_empty());
        assert!(commit_rights_note(Some(&serde_json::json!({"other": "field"}))).is_empty());
    }

    #[test]
    fn interrupted_note_renders_for_interrupted_run() {
        let output = Some(serde_json::json!({"interrupted": true}));
        let note = interrupted_note(output.as_ref());
        assert!(note.contains("interrupted"));
        assert!(!note.contains("git stash"));
    }

    #[test]
    fn interrupted_note_empty_when_not_interrupted() {
        assert!(interrupted_note(None).is_empty());
        assert!(interrupted_note(Some(&serde_json::json!({"interrupted": false}))).is_empty());
    }

    #[test]
    fn format_dt_renders_local_time() {
        let dt = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let formatted = format_dt(dt);
        // The function converts to local time, so we just check it's non-empty and has a colon
        assert!(!formatted.is_empty());
        assert!(formatted.contains(":"));
    }

    #[test]
    fn candidate_list_renders_graph_names() {
        let graphs = [
            make_graph("abc123", "graph-one", GraphStatus::Draft),
            make_graph("def456", "graph-two", GraphStatus::Running),
        ];
        let list = candidate_list(graphs.iter());
        assert!(list.contains("graph-one"));
        assert!(list.contains("graph-two"));
        assert!(list.contains("abc123"));
        assert!(list.contains("def456"));
    }

    #[test]
    fn not_found_error_mentions_query() {
        let graphs = [make_graph("abc123", "one", GraphStatus::Draft)];
        let err = not_found_error("missing", &graphs);
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn ambiguous_error_mentions_all_candidates() {
        let graphs = [
            make_graph("abc123", "dup", GraphStatus::Draft),
            make_graph("def456", "dup", GraphStatus::Draft),
        ];
        let matches: Vec<&Graph> = graphs.iter().collect();
        let err = ambiguous_error("dup", &matches);
        let msg = err.to_string();
        assert!(msg.contains("abc123"));
        assert!(msg.contains("def456"));
    }

    #[test]
    fn current_spec_name_returns_none_for_empty_specs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let lp = make_graph("graph1", "test-graph", GraphStatus::Running);
        let result = current_spec_name(&db, &lp, &[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn current_spec_name_returns_name_for_running_spec() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let lp = make_graph("graph1", "test-graph", GraphStatus::Running);
        let specs = vec![
            make_spec("graph1", "spec-a", 0, GraphSpecStatus::Completed),
            make_spec("graph1", "spec-b", 1, GraphSpecStatus::Running),
        ];
        let result = current_spec_name(&db, &lp, &specs).unwrap();
        assert_eq!(result, Some("spec-b".to_string()));
    }

    #[test]
    fn graph_surfaces_never_show_blank_name_spec() {
        // CB30-4: for every dispatch shape (bound, queue, idea) the shared
        // helper and the `graph list`/`graph info` name lookup never yield a
        // blank name — the regression is an automated reader concluding
        // "this graph is on a nameless spec" while it works a named one.
        use crate::domain::queues::Queue;

        fn assert_no_blank(name: Option<String>, shape: &str) {
            if let Some(name) = name {
                assert!(
                    !name.trim().is_empty(),
                    "{shape} surface named a blank spec"
                );
            }
        }

        // Queue-driven: placeholder bound to the graph, named member running.
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let mut lp = make_graph("graph-q", "queue-graph", GraphStatus::Running);
        lp.active_run_queue_id = Some("queue-1".to_string());
        db.insert_graph(&lp).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "Q".to_string(),
            created_at: Utc::now(),
        })
        .unwrap();
        let mut ph = make_spec("graph-q", "", 0, GraphSpecStatus::Running);
        db.insert_graph_spec(&ph).unwrap();
        ph.id = "spec-a".to_string();
        ph.graph_id = None;
        ph.name = "QM — real work".to_string();
        db.insert_graph_spec(&ph).unwrap();
        db.append_queue_member("queue-1", "spec-a", None).unwrap();

        match active_graph_spec_context(&db, &lp).unwrap() {
            ActiveGraphSpec::QueueMember { spec, .. } => {
                assert!(!spec.name.trim().is_empty());
                assert_eq!(spec.name, "QM — real work");
            }
            _ => panic!("queue-driven graph must resolve to QueueMember"),
        }
        let bound = db.list_graph_specs("graph-q").unwrap();
        assert_no_blank(current_spec_name(&db, &lp, &bound).unwrap(), "queue");
        // The placeholder itself must never be picked as "current".
        assert!(current_spec(&bound).is_none_or(|s| !s.name.trim().is_empty()));

        // Idea-driven: only the placeholder exists.
        let mut lp_idea = make_graph("graph-idea", "idea-graph", GraphStatus::Running);
        lp_idea.id = "graph-idea".to_string();
        db.insert_graph(&lp_idea).unwrap();
        db.insert_graph_spec(&make_spec("graph-idea", "", 0, GraphSpecStatus::Running))
            .unwrap();
        assert!(matches!(
            active_graph_spec_context(&db, &lp_idea).unwrap(),
            ActiveGraphSpec::Idea { .. }
        ));
        let bound_idea = db.list_graph_specs("graph-idea").unwrap();
        assert_no_blank(
            current_spec_name(&db, &lp_idea, &bound_idea).unwrap(),
            "idea",
        );

        // Bound: named specs behave exactly as before.
        let lp_bound = make_graph("graph-bound", "bound-graph", GraphStatus::Running);
        db.insert_graph(&lp_bound).unwrap();
        db.insert_graph_spec(&make_spec(
            "graph-bound",
            "Real work",
            0,
            GraphSpecStatus::Running,
        ))
        .unwrap();
        let bound_specs = db.list_graph_specs("graph-bound").unwrap();
        assert_eq!(
            current_spec_name(&db, &lp_bound, &bound_specs).unwrap(),
            Some("Real work".to_string())
        );
    }

    #[test]
    fn format_elapsed_zero_seconds() {
        assert_eq!(format_elapsed(chrono::Duration::seconds(0)), "0s");
    }

    #[test]
    fn format_elapsed_exactly_one_minute() {
        assert_eq!(format_elapsed(chrono::Duration::seconds(60)), "1m0s");
    }

    #[test]
    fn format_elapsed_exactly_one_hour() {
        assert_eq!(format_elapsed(chrono::Duration::seconds(3600)), "1h0m");
    }

    #[test]
    fn format_relative_duration_exactly_zero() {
        assert_eq!(
            format_relative_duration(chrono::Duration::seconds(0)),
            "due now"
        );
    }

    #[test]
    fn format_relative_duration_large_future() {
        let result = format_relative_duration(chrono::Duration::days(365));
        assert!(result.contains("in"));
    }

    #[test]
    fn format_autorun_compact_midnight() {
        let at = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let result = format_autorun_compact(at);
        assert!(result.contains("00:00Z"));
    }

    #[test]
    fn format_autorun_compact_end_of_day() {
        let at = DateTime::<Utc>::from_timestamp(23 * 3600 + 59 * 60, 0).unwrap();
        let result = format_autorun_compact(at);
        assert!(result.contains("23:59Z"));
    }

    // ── state-changing handlers: daemon delegation ──────────────────

    use crate::tui::mcp_client::test_support::{spawn_fake_daemon, unused_port};

    fn db_with_graph(id: &str, name: &str, status: GraphStatus) -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        db.insert_graph(&make_graph(id, name, status)).unwrap();
        (dir, db)
    }

    #[test]
    fn handle_graph_run_calls_graph_run_tool_with_resolved_id() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "Graph 'graph-1' launched in background."}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_run(
            &db,
            Some(port),
            "my-graph",
            Some("q1".to_string()),
            None,
            None,
            None,
        )
        .expect("run should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "graph_run");
        assert_eq!(calls[0]["arguments"]["graph_id"], "graph-1");
        assert_eq!(calls[0]["arguments"]["queue_id"], "q1");
    }

    #[test]
    fn handle_graph_run_rejects_unknown_graph_before_touching_daemon() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        // No fake daemon is spawned — a request that reaches the network at
        // all would fail differently (connection refused) than the
        // not-found error resolution must produce locally.
        let err =
            handle_graph_run(&db, Some(65535), "missing", None, None, None, None).unwrap_err();
        assert!(err.to_string().contains("No graph matches"));
    }

    #[test]
    fn handle_graph_pause_calls_graph_pause_tool() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Running);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "Graph 'graph-1' marked to pause."}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_pause(&db, Some(port), "graph-1").expect("pause should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "graph_pause");
        assert_eq!(calls[0]["arguments"]["graph_id"], "graph-1");
    }

    #[test]
    fn handle_graph_continue_sends_retry_current_node_action() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Paused);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "resumed"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_continue(&db, Some(port), "graph-1", true, false, None, None)
            .expect("should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["arguments"]["action"], "retry_current_node");
    }

    #[test]
    fn handle_graph_continue_sends_skip_next_spec_action() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Paused);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "resumed"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_continue(&db, Some(port), "graph-1", false, true, None, None)
            .expect("should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["arguments"]["action"], "skip_next_spec");
    }

    #[test]
    fn handle_graph_continue_rejects_when_neither_flag_set() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Paused);
        let err = handle_graph_continue(&db, Some(65535), "graph-1", false, false, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("Specify one of"));
    }

    #[test]
    fn handle_graph_continue_rejects_when_both_flags_set() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Paused);
        let err =
            handle_graph_continue(&db, Some(65535), "graph-1", true, true, None, None).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn handle_graph_reset_with_yes_sends_explicit_specs() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Failed);
        // `yes: true` bypasses the interactive confirmation prompt (which
        // would otherwise block on a non-tty test run), exercising the
        // confirmed path deterministically.
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "Graph 'graph-1' reset."}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_reset(
            &db,
            Some(port),
            "graph-1",
            &["spec-a".to_string(), "spec-b".to_string()],
            true,
        )
        .expect("reset should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "graph_reset");
        assert_eq!(calls[0]["arguments"]["graph_id"], "graph-1");
        assert_eq!(
            calls[0]["arguments"]["specs"],
            serde_json::json!(["spec-a", "spec-b"])
        );
    }

    #[test]
    fn handle_graph_reset_omits_specs_field_when_none_given() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Failed);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "Graph 'graph-1' reset."}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_reset(&db, Some(port), "graph-1", &[], true).expect("reset should succeed");

        let calls = fake.recorded_calls();
        assert!(calls[0]["arguments"].get("specs").is_none());
    }

    /// Export is read-only and must work without any daemon listening —
    /// mirroring `list`/`info`, it reads the local database directly.
    #[test]
    fn handle_graph_export_writes_document_to_output_path_without_a_daemon() {
        let (dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        db.insert_graph_node(&crate::domain::graphs::GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: "implementer".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "go"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();

        let out_path = dir.path().join("export.json");
        handle_graph_export(&db, "graph-1", Some(out_path.to_str().unwrap()))
            .expect("export should succeed");

        let raw = std::fs::read_to_string(&out_path).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["format_version"], 4);
        assert_eq!(doc["name"], "my-graph");
        assert_eq!(doc["nodes"][0]["config"]["platform"], "claude");
        // No model stored (platform default): exported as explicit null.
        assert!(doc["nodes"][0]["config"].get("model").is_some());
        assert!(doc["nodes"][0]["config"]["model"].is_null());
    }

    #[test]
    fn handle_graph_export_rejects_unknown_graph_before_touching_disk() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let err = handle_graph_export(&db, "missing", None).unwrap_err();
        assert!(err.to_string().contains("No graph matches"));
    }

    /// Import delegates to the daemon's `graph_import` MCP tool — unlike
    /// export, it has state-changing side effects (project registration,
    /// trigger activation) that only the running daemon can perform.
    #[test]
    fn handle_graph_import_sends_document_workdir_and_name_to_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("graph.json");
        std::fs::write(
            &file_path,
            serde_json::json!({
                "format_version": 1,
                "name": "Shared Graph",
                "nodes": [],
                "edges": [],
                "ensembles": []
            })
            .to_string(),
        )
        .unwrap();

        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{\"graph_id\": \"new-1\", \"name\": \"Shared Graph\", \"nodes_missing_platform\": []}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_import(
            Some(port),
            file_path.to_str().unwrap(),
            Some("/tmp/target".to_string()),
            Some("Custom Name".to_string()),
        )
        .expect("import should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "graph_import");
        assert_eq!(calls[0]["arguments"]["workdir"], "/tmp/target");
        assert_eq!(calls[0]["arguments"]["name"], "Custom Name");
        assert_eq!(calls[0]["arguments"]["document"]["name"], "Shared Graph");
    }

    #[test]
    fn handle_graph_import_rejects_missing_format_version_before_touching_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("graph.json");
        std::fs::write(
            &file_path,
            serde_json::json!({"name": "x", "nodes": [], "edges": [], "ensembles": []}).to_string(),
        )
        .unwrap();

        // No fake daemon spawned — a request that reaches the network at
        // all would fail differently (connection refused).
        let err =
            handle_graph_import(Some(65535), file_path.to_str().unwrap(), None, None).unwrap_err();
        assert!(err.to_string().contains("format_version"));
    }

    #[test]
    fn handle_graph_import_rejects_unreadable_path() {
        let err =
            handle_graph_import(Some(65535), "/nonexistent/graph.json", None, None).unwrap_err();
        assert!(err.to_string().contains("could not read"));
    }

    #[test]
    fn handle_graph_autorun_sends_at() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "scheduled"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_autorun(
            &db,
            Some(port),
            "graph-1",
            Some("2026-07-10T09:00:00Z"),
            None,
            false,
        )
        .expect("autorun should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["arguments"]["at"], "2026-07-10T09:00:00Z");
    }

    #[test]
    fn handle_graph_autorun_cancel_omits_at_and_message() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "cancelled"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        handle_graph_autorun(&db, Some(port), "graph-1", None, None, true)
            .expect("autorun cancel should succeed");

        let calls = fake.recorded_calls();
        assert!(calls[0]["arguments"].get("at").is_none());
        assert!(calls[0]["arguments"].get("quota_reset_message").is_none());
    }

    #[test]
    fn handle_graph_autorun_rejects_no_flags() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let err = handle_graph_autorun(&db, Some(65535), "graph-1", None, None, false).unwrap_err();
        assert!(err.to_string().contains("Specify one of"));
    }

    #[test]
    fn handle_graph_autorun_rejects_multiple_flags() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Draft);
        let err = handle_graph_autorun(
            &db,
            Some(65535),
            "graph-1",
            Some("2026-07-10T09:00:00Z"),
            None,
            true,
        )
        .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    /// The case this spec exists for: the daemon is unreachable on the
    /// resolved port, and the failure must read as "daemon unreachable",
    /// never as "the graph does not exist" (the graph resolves fine locally
    /// against the same database `graph list`/`graph info` already read).
    #[test]
    fn handle_graph_pause_reports_daemon_unreachable_distinctly_from_not_found() {
        let (_dir, db) = db_with_graph("graph-1", "my-graph", GraphStatus::Running);

        for _ in 0..5 {
            let port: u16 = unused_port().parse().unwrap();
            match handle_graph_pause(&db, Some(port), "graph-1") {
                Err(err) => {
                    let chain: Vec<String> = err.chain().map(ToString::to_string).collect();
                    assert!(chain
                        .iter()
                        .any(|m| m.contains("could not reach the daemon")));
                    assert!(!chain.iter().any(|m| m.contains("No graph matches")));
                    return;
                }
                Ok(()) => continue,
            }
        }
        panic!("port kept getting claimed by another test after 5 attempts");
    }

    #[test]
    fn handle_graph_info_covers_bound_specs_branch() {
        let (_dir, db) = db_with_graph("graph-1", "bound-graph", GraphStatus::Draft);
        let mut spec = make_spec("graph-1", "spec-a", 0, GraphSpecStatus::Pending);
        spec.id = "graph-1-0".to_string();
        db.insert_graph_spec(&spec).unwrap();
        handle_graph_info(&db, "bound-graph").expect("bound info must print");
    }

    #[test]
    fn handle_graph_info_covers_idea_branch() {
        let (_dir, db) = db_with_graph("graph-2", "idea-graph", GraphStatus::Draft);
        let mut placeholder = make_spec("graph-2", "", 0, GraphSpecStatus::Pending);
        placeholder.id = "graph-2-0".to_string();
        db.insert_graph_spec(&placeholder).unwrap();
        handle_graph_info(&db, "idea-graph").expect("idea info must print");
    }

    #[test]
    fn handle_graph_info_covers_queue_member_branch() {
        use crate::domain::queues::Queue;
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        let mut lp = make_graph("graph-3", "queue-graph", GraphStatus::Running);
        lp.active_run_queue_id = Some("queue-1".to_string());
        db.insert_graph(&lp).unwrap();
        db.insert_queue(&Queue {
            id: "queue-1".to_string(),
            name: "Q".to_string(),
            created_at: Utc::now(),
        })
        .unwrap();
        let mut member = make_spec("queue", "real work", 0, GraphSpecStatus::Running);
        member.id = "spec-a".to_string();
        member.graph_id = None;
        db.insert_graph_spec(&member).unwrap();
        db.append_queue_member("queue-1", "spec-a", None).unwrap();
        handle_graph_info(&db, "queue-graph").expect("queue info must print");
    }

    /// Plan §5: a section-level capture for `handle_graph_info`, asserting
    /// exact section order and the key lines of each mode it routes to.
    #[test]
    fn handle_graph_info_output_captures_bound_mode_sections_in_order() {
        let (_dir, db) = db_with_graph("graph-c1", "cap-bound", GraphStatus::Draft);
        let mut spec = make_spec("graph-c1", "spec-a", 0, GraphSpecStatus::Pending);
        spec.id = "graph-c1-0".to_string();
        db.insert_graph_spec(&spec).unwrap();

        let cap = StdoutCapture::start();
        handle_graph_info(&db, "cap-bound").expect("bound info must print");
        let out = cap.stop();

        assert_ordered(
            &out,
            &[
                "── Graph: cap-bound ──",
                " id:      graph-c1",
                "── Specs ──",
                "spec-a",
                "\n",
            ],
        );
        assert!(
            !out.contains("── Current Node ──"),
            "draft graph has no current node"
        );
        assert!(!out.contains("── Recent Runs ──"), "no runs recorded yet");
    }

    /// Plan §5: the idea mode's single placeholder line, plus the
    /// "no specs queued" queue-history fallback for a drained queue.
    #[test]
    fn handle_graph_info_output_captures_idea_and_queue_history_modes() {
        let (_dir, db) = db_with_graph("graph-c2", "cap-idea", GraphStatus::Draft);
        let mut placeholder = make_spec("graph-c2", "", 0, GraphSpecStatus::Pending);
        placeholder.id = "graph-c2-0".to_string();
        db.insert_graph_spec(&placeholder).unwrap();

        let cap = StdoutCapture::start();
        handle_graph_info(&db, "cap-idea").expect("idea info must print");
        let idea_out = cap.stop();
        assert!(idea_out.contains("── Specs ──"));
        assert!(
            idea_out.contains("(idea-driven — no bound specs)"),
            "idea mode prints its placeholder line, got {idea_out:?}"
        );

        // Queue history: a graph with nothing bound but a recorded run.
        let mut lp = make_graph("graph-c3", "cap-history", GraphStatus::Completed);
        lp.id = "graph-c3".to_string();
        let dir = tempfile::tempdir().unwrap();
        let db2 = Database::new(&dir.path().join("h.db")).unwrap();
        db2.insert_graph(&lp).unwrap();
        let mut ran = make_spec("graph-c3", "earlier work", 0, GraphSpecStatus::Pending);
        ran.id = "spec-hist".to_string();
        db2.insert_graph_spec(&ran).unwrap();
        let node = crate::domain::graphs::GraphNode {
            id: "node-hist".to_string(),
            spec_id: None,
            graph_id: Some("graph-c3".to_string()),
            name: "check".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 0,
            created_at: Utc::now(),
        };
        db2.insert_graph_node(&node).unwrap();
        let mut hist_run = make_run(
            "spec-hist",
            "node-hist",
            GraphRunStatus::Pass,
            1_700_000_000,
        );
        hist_run.id = "run-hist".to_string();
        hist_run.graph_id = "graph-c3".to_string();
        hist_run.completed_at = Some(Utc::now());
        db2.insert_graph_run(&hist_run).unwrap();

        let cap = StdoutCapture::start();
        handle_graph_info(&db2, "cap-history").expect("queue-history info must print");
        let hist_out = cap.stop();
        assert_ordered(&hist_out, &["── Specs ──", "earlier work"]);
    }

    #[test]
    fn print_graph_info_hook_runs_covers_empty_and_populated() {
        print_graph_info_hook_runs(&[]);
        let run = crate::domain::graphs::GraphCompletionHookRun {
            id: "run-1".to_string(),
            graph_id: "graph-1".to_string(),
            event: crate::domain::graphs::GraphHookEvent::OnCompleted,
            hook_index: 0,
            status: GraphRunStatus::Pass,
            output: None,
            summary: None,
            started_at: Utc::now(),
            completed_at: None,
            pid: None,
            boot_id: None,
            executed_platform: None,
            executed_model: None,
            hook_config: None,
        };
        print_graph_info_hook_runs(std::slice::from_ref(&run));
    }

    #[test]
    fn print_graph_info_current_node_covers_idle_and_recent_runs_empty() {
        let lp = make_graph("graph-9", "idle-graph", GraphStatus::Draft);
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("t.db")).unwrap();
        print_graph_info_current_node(&db, &lp, &[]).expect("idle current node");
        print_graph_info_recent_runs(&db, &lp, &[]).expect("empty recent runs");
    }
}
