//! CLI for `canopy node` — install, list, and remove shareable node
//! documents.
//!
//! Unlike `canopy graph install` (which hands the import to the daemon
//! because it writes the database), a node document is plain files under
//! `~/.canopy`, so all three commands work locally with no daemon running.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use crate::domain::nodes::{self, NodeEntry, RemoveOutcome};

#[derive(Subcommand, Debug)]
pub(crate) enum NodeAction {
    /// List every resolvable node document, marking installed copies and
    /// builtins embedded in the binary.
    List {
        /// Print the machine-readable list instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },
    /// Install a node document from the catalogue — fetch the published
    /// document for the ref, validate it, install its preset, and write it
    /// to ~/.canopy/nodes.
    Install {
        /// Catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
        /// optional `@<ref>` suffix.
        reference: String,
        /// Overwrite a local preset file whose content differs instead of
        /// refusing the install.
        #[arg(long = "overwrite-presets")]
        overwrite_presets: bool,
        /// Print the machine-readable report instead of human-readable lines.
        #[arg(long)]
        json: bool,
    },
    /// Remove an installed node document. A builtin with no installed copy
    /// cannot be removed; an installed override of a builtin can.
    Remove {
        /// Node name.
        name: String,
        /// Print the machine-readable result instead of a human-readable line.
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn handle_node_action(action: NodeAction) -> Result<()> {
    match action {
        NodeAction::List { json } => list(json),
        NodeAction::Install {
            reference,
            overwrite_presets,
            json,
        } => install(&reference, overwrite_presets, json),
        NodeAction::Remove { name, json } => remove(&name, json),
    }
}

fn nodes_dir() -> std::path::PathBuf {
    nodes::nodes_dir(&crate::domain::prompts::canopy_dir())
}

fn list(json: bool) -> Result<()> {
    let entries = nodes::list(&nodes_dir()).map_err(|e| anyhow!(e))?;
    if json {
        println!("{}", serde_json::json!({ "nodes": list_rows(&entries) }));
        return Ok(());
    }
    if entries.is_empty() {
        println!("No node documents found.");
        return Ok(());
    }
    for entry in &entries {
        println!(
            "{}  {:<8} {}",
            entry.document.name,
            entry.document.kind,
            entry.source.as_str()
        );
    }
    Ok(())
}

fn list_rows(entries: &[NodeEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "name": entry.document.name,
                "kind": entry.document.kind,
                "source": entry.source.as_str(),
            })
        })
        .collect()
}

fn install(reference: &str, overwrite_presets: bool, json: bool) -> Result<()> {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let prepared = crate::daemon::node_install::prepare_node_install(
        &crate::daemon::graph_install::catalogue_url(),
        reference,
        overwrite_presets,
        &nodes::nodes_dir(&canopy_dir),
        &crate::domain::prompts::prompts_dir(&canopy_dir),
    )
    .map_err(|e| anyhow!(e))?;
    let report = serde_json::json!({
        "name": prepared.document.name,
        "kind": prepared.document.kind,
        "ref": reference,
        "source_url": prepared.source_url,
        "presets_written": prepared.presets_written,
    });
    if json {
        println!("{report}");
    } else {
        for line in install_report_lines(&report) {
            println!("{line}");
        }
    }
    Ok(())
}

/// Human-readable lines for a `node install` report: what was installed,
/// the presets written, and the call that adds it to a graph.
fn install_report_lines(report: &serde_json::Value) -> Vec<String> {
    let field = |key: &str| {
        report
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?")
    };
    let name = field("name");
    let kind = field("kind");
    let mut lines = vec![format!(
        "Installed node '{name}' ({kind}) from {}.",
        field("source_url")
    )];
    let presets = report
        .get("presets_written")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    lines.push(if presets.is_empty() {
        "presets_written: none".to_string()
    } else {
        format!("presets_written: {}", presets.join(", "))
    });
    let add = if kind == "ensemble" {
        "graph_add_ensemble"
    } else {
        "graph_add_node"
    };
    lines.push(format!(
        "next: add it to a graph with {add} (node: \"{name}\")."
    ));
    lines
}

fn remove(name: &str, json: bool) -> Result<()> {
    let dir = nodes_dir();
    match nodes::remove(&dir, name.trim()).map_err(|e| anyhow!(e))? {
        RemoveOutcome::Removed => {
            if json {
                println!("{}", serde_json::json!({"name": name, "removed": true}));
            } else {
                println!("Removed node '{name}'.");
            }
            Ok(())
        }
        RemoveOutcome::BuiltinProtected => Err(anyhow!(
            "Node '{name}' is a builtin and has no installed copy to remove, so it cannot be removed. Install your own copy with `canopy node install` to override it; that installed copy can then be removed."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: NodeAction,
    }

    #[test]
    fn list_parses_with_and_without_json() {
        let cli = TestCli::try_parse_from(["test", "list"]).expect("list parses");
        assert!(matches!(cli.action, NodeAction::List { json: false }));
        let cli = TestCli::try_parse_from(["test", "list", "--json"]).expect("list --json parses");
        assert!(matches!(cli.action, NodeAction::List { json: true }));
    }

    #[test]
    fn install_parses_ref_overwrite_and_json() {
        let cli = TestCli::try_parse_from([
            "test",
            "install",
            "acme/widgets/auditor@v1",
            "--overwrite-presets",
            "--json",
        ])
        .expect("install parses");
        match cli.action {
            NodeAction::Install {
                reference,
                overwrite_presets,
                json,
            } => {
                assert_eq!(reference, "acme/widgets/auditor@v1");
                assert!(overwrite_presets);
                assert!(json);
            }
            other => panic!("expected Install, got {other:?}"),
        }
    }

    #[test]
    fn remove_requires_a_name() {
        assert!(TestCli::try_parse_from(["test", "remove"]).is_err());
        let cli = TestCli::try_parse_from(["test", "remove", "implementer", "--json"])
            .expect("remove parses");
        match cli.action {
            NodeAction::Remove { name, json } => {
                assert_eq!(name, "implementer");
                assert!(json);
            }
            other => panic!("expected Remove, got {other:?}"),
        }
    }

    #[test]
    fn install_report_lists_name_kind_presets_and_the_add_call() {
        let report = serde_json::json!({
            "name": "auditor",
            "kind": "agent",
            "ref": "auditor",
            "source_url": "https://graphs.univerlab.org/i/node/auditor",
            "presets_written": ["auditor-preset"],
        });
        let lines = install_report_lines(&report);
        assert!(
            lines
                .iter()
                .any(|line| line.contains("auditor") && line.contains("agent")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("auditor-preset")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("graph_add_node") && line.contains("auditor")),
            "{lines:?}"
        );
        let ensemble = install_report_lines(&serde_json::json!({
            "name": "ensemble-proposers",
            "kind": "ensemble",
            "source_url": "u",
            "presets_written": [],
        }));
        assert!(
            ensemble
                .iter()
                .any(|line| line.contains("graph_add_ensemble")),
            "{ensemble:?}"
        );
        assert!(
            ensemble
                .iter()
                .any(|line| line.contains("presets_written: none")),
            "{ensemble:?}"
        );
    }
}
