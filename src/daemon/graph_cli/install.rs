//! `canopy graph install`: fetch a published graph from the catalogue and
//! import it — one command from ref to local graph. Like `import`, this
//! changes graph state, so it delegates to the daemon's `graph_install`
//! MCP tool rather than writing to the database directly.

use anyhow::{Context, Result};

/// Arguments for `canopy graph install`, flattened into the `Install`
/// action so the dispatch arm stays one line.
#[derive(clap::Args, Debug)]
pub(crate) struct InstallArgs {
    /// Catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
    /// optional `@<ref>` suffix.
    reference: String,
    /// Absolute workdir for the new graph. Defaults to the current
    /// directory.
    #[arg(long)]
    workdir: Option<String>,
    /// Graph name to use instead of the document's own name.
    #[arg(long)]
    name: Option<String>,
    /// Overwrite a local preset file whose content differs instead of
    /// refusing the install.
    #[arg(long = "overwrite-presets")]
    overwrite_presets: bool,
    /// Print the machine-readable report instead of human-readable lines.
    #[arg(long)]
    json: bool,
}

/// Install the catalogue ref: default the workdir to the current directory,
/// call the daemon's `graph_install` tool, and print the report (`--json`
/// for the machine shape).
pub(super) fn handle_graph_install(port_override: Option<u16>, args: InstallArgs) -> Result<()> {
    let InstallArgs {
        reference,
        workdir,
        name,
        overwrite_presets,
        json,
    } = args;
    let workdir = match workdir {
        Some(workdir) => workdir,
        None => std::env::current_dir()
            .context("could not determine the current directory")?
            .to_string_lossy()
            .to_string(),
    };
    let args = install_args(&reference, &workdir, name.as_deref(), overwrite_presets);
    let text = crate::daemon::cli_daemon::call_tool(port_override, "graph_install", &args)?;
    if json {
        println!("{text}");
    } else if let Ok(report) = serde_json::from_str::<serde_json::Value>(&text) {
        print_install_report(&report);
    } else {
        println!("{text}");
    }
    Ok(())
}

/// The `graph_install` arguments for a CLI invocation.
fn install_args(
    reference: &str,
    workdir: &str,
    name: Option<&str>,
    overwrite_presets: bool,
) -> serde_json::Value {
    let mut args = serde_json::json!({ "ref": reference, "workdir": workdir });
    if let Some(name) = name {
        args["name"] = serde_json::json!(name);
    }
    if overwrite_presets {
        args["overwrite_presets"] = serde_json::json!(true);
    }
    args
}

fn print_install_report(report: &serde_json::Value) {
    for line in install_report_lines(report) {
        println!("{line}");
    }
}

/// Human-readable lines for a `graph_install` report: graph id and name,
/// workdir, presets written, and the agent nodes still needing a
/// platform/model with the next command to assign them.
fn install_report_lines(report: &serde_json::Value) -> Vec<String> {
    let field = |key: &str| {
        report
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    };
    let list = |key: &str| {
        report
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mut lines = vec![format!(
        "Installed graph '{}' ({}) into {}.",
        field("name"),
        field("graph_id"),
        field("workdir")
    )];
    let presets = list("presets_written");
    lines.push(if presets.is_empty() {
        "presets_written: none".to_string()
    } else {
        format!("presets_written: {}", presets.join(", "))
    });
    let missing = list("nodes_missing_platform");
    lines.push(if missing.is_empty() {
        "nodes_missing_platform: none (every agent node has a harness)".to_string()
    } else {
        format!("nodes_missing_platform: {}", missing.join(", "))
    });
    lines.push(format!("next: {}", field("next")));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::mcp_client::test_support::spawn_fake_daemon;

    fn install_report() -> serde_json::Value {
        serde_json::json!({
            "graph_id": "graph-1",
            "name": "installed",
            "workdir": "/tmp/work",
            "ref": "my-graph",
            "source_url": "https://graphs.univerlab.org/i/graph/my-graph",
            "presets_written": ["my-auditor"],
            "nodes_missing_platform": ["scout", "builder"],
            "missing_crews": [],
            "terminals": [],
            "next": "Assign a platform/model to scout, builder with graph_update_node, then start the graph with graph_run (graph_id graph-1)."
        })
    }

    #[test]
    fn install_report_lists_graph_id_name_and_workdir() {
        let lines = install_report_lines(&install_report());
        assert!(
            lines.iter().any(|line| line.contains("installed")
                && line.contains("graph-1")
                && line.contains("/tmp/work")),
            "{lines:?}"
        );
    }

    #[test]
    fn install_report_lists_presets_written() {
        let lines = install_report_lines(&install_report());
        assert!(
            lines.iter().any(|line| line.contains("my-auditor")),
            "{lines:?}"
        );
        let empty = install_report_lines(&serde_json::json!({
            "graph_id": "g", "name": "n", "workdir": "/w",
            "presets_written": [], "nodes_missing_platform": [], "next": "run it",
        }));
        assert!(
            empty
                .iter()
                .any(|line| line.contains("presets_written: none")),
            "{empty:?}"
        );
    }

    #[test]
    fn install_report_lists_nodes_without_harness_and_the_next_command() {
        let lines = install_report_lines(&install_report());
        assert!(
            lines
                .iter()
                .any(|line| line.contains("scout") && line.contains("builder")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("graph_update_node") && line.contains("graph_run")),
            "{lines:?}"
        );
    }

    #[test]
    fn install_sends_ref_workdir_name_and_overwrite_to_the_daemon() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        handle_graph_install(
            Some(port),
            InstallArgs {
                reference: "acme/widgets/my-graph@v1.2".to_string(),
                workdir: Some("/tmp/work".to_string()),
                name: Some("renamed".to_string()),
                overwrite_presets: true,
                json: true,
            },
        )
        .unwrap();
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "graph_install");
        assert_eq!(calls[0]["arguments"]["ref"], "acme/widgets/my-graph@v1.2");
        assert_eq!(calls[0]["arguments"]["workdir"], "/tmp/work");
        assert_eq!(calls[0]["arguments"]["name"], "renamed");
        assert_eq!(calls[0]["arguments"]["overwrite_presets"], true);
    }

    #[test]
    fn install_omits_name_and_overwrite_when_not_given() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        handle_graph_install(
            Some(port),
            InstallArgs {
                reference: "my-graph".to_string(),
                workdir: Some("/tmp/work".to_string()),
                name: None,
                overwrite_presets: false,
                json: true,
            },
        )
        .unwrap();
        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["arguments"]["ref"], "my-graph");
        assert!(calls[0]["arguments"].get("name").is_none());
        assert!(calls[0]["arguments"].get("overwrite_presets").is_none());
    }

    /// Without `--workdir`, the install targets the directory the command was
    /// run from — absolute, so the daemon's absolute-and-exists check passes.
    #[test]
    fn install_defaults_the_workdir_to_the_current_directory() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        handle_graph_install(
            Some(port),
            InstallArgs {
                reference: "my-graph".to_string(),
                workdir: None,
                name: None,
                overwrite_presets: false,
                json: true,
            },
        )
        .unwrap();
        let expected = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(std::path::Path::new(&expected).is_absolute());
        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["arguments"]["workdir"], expected);
    }
}
