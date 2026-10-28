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
    /// Install a node document from the catalogue — fetch and validate the
    /// published document for the ref, show what it runs, ask for
    /// confirmation, then install its preset and write it to
    /// ~/.canopy/nodes.
    Install {
        /// Catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
        /// optional `@<ref>` suffix — or `draft:<id>` for an editor draft.
        reference: String,
        /// Overwrite a local preset file whose content differs instead of
        /// refusing the install.
        #[arg(long = "overwrite-presets")]
        overwrite_presets: bool,
        /// Install without the confirmation question (the review summary is
        /// still printed).
        #[arg(long)]
        yes: bool,
        /// Print every prompt in the review in full instead of its first 3
        /// lines.
        #[arg(long = "show-prompts")]
        show_prompts: bool,
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
            yes,
            show_prompts,
            json,
        } => {
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdin())
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            run_install(
                &reference,
                overwrite_presets,
                yes,
                show_prompts,
                json,
                tty,
                &mut std::io::stdin().lock(),
            )
        }
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

/// The `canopy node install` flow, with `tty` and the answer reader injected
/// so tests can drive it: fetch and validate the document (nothing written),
/// print the summary (`--json` prints the review as JSON, the question goes
/// to stderr), ask `Install <name> from <source>? [y/N]` unless `--yes`, and
/// only then write the preset and the node — finally print the report with
/// the review it was asked about.
pub(crate) fn run_install(
    reference: &str,
    overwrite_presets: bool,
    yes: bool,
    show_prompts: bool,
    json: bool,
    tty: bool,
    read: &mut impl std::io::BufRead,
) -> Result<()> {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let fetched = crate::daemon::node_install::fetch_node_document(
        &crate::daemon::graph_install::catalogue_url(),
        reference,
    )
    .map_err(|e| anyhow!(e))?;
    let review = crate::domain::nodes::review::review_node(&fetched.document, &fetched.source);
    print_summary(&review, show_prompts, json);

    let is_draft = crate::daemon::graph_install::draft_id(reference).is_some();
    let ask = crate::daemon::install_confirm::Ask {
        name: review.name.clone(),
        source: review.source.clone(),
        yes,
        tty,
        to_stderr: json,
        is_draft,
    };
    ask.confirm(read).map_err(|message| {
        if is_draft {
            anyhow!(
                "{message}{}",
                crate::daemon::install_confirm::draft_consumed_note()
            )
        } else {
            anyhow!(message)
        }
    })?;

    let presets_written = crate::daemon::node_install::write_node_install(
        &fetched.document,
        overwrite_presets,
        &nodes::nodes_dir(&canopy_dir),
        &crate::domain::prompts::prompts_dir(&canopy_dir),
    )
    .map_err(|e| anyhow!(e))?;
    let report = serde_json::json!({
        "name": fetched.document.name,
        "kind": fetched.document.kind,
        "ref": reference,
        "source_url": fetched.source_url,
        "source": fetched.source,
        "presets_written": presets_written,
        "review": review,
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

/// The summary before the question: rendered text on stdout in human mode,
/// the review as JSON on stdout in `--json` mode — never skipped, even when
/// the install is then refused for having no TTY.
fn print_summary(
    review: &crate::domain::nodes::review::InstallReview,
    show_prompts: bool,
    json: bool,
) {
    if json {
        match serde_json::to_string(review) {
            Ok(text) => println!("{text}"),
            Err(_) => println!(
                "{}",
                crate::domain::nodes::review::render(review, show_prompts)
            ),
        }
    } else {
        println!(
            "{}",
            crate::domain::nodes::review::render(review, show_prompts)
        );
    }
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
    let mut lines = vec![
        format!(
            "Installed node '{name}' ({kind}) from {}.",
            field("source_url")
        ),
        format!("source: {}", field("source")),
    ];
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
    fn install_parses_ref_overwrite_yes_show_prompts_and_json() {
        let cli = TestCli::try_parse_from([
            "test",
            "install",
            "acme/widgets/auditor@v1",
            "--overwrite-presets",
            "--yes",
            "--show-prompts",
            "--json",
        ])
        .expect("install parses");
        match cli.action {
            NodeAction::Install {
                reference,
                overwrite_presets,
                yes,
                show_prompts,
                json,
            } => {
                assert_eq!(reference, "acme/widgets/auditor@v1");
                assert!(overwrite_presets);
                assert!(yes);
                assert!(show_prompts);
                assert!(json);
            }
            other => panic!("expected Install, got {other:?}"),
        }
        let cli = TestCli::try_parse_from(["test", "install", "auditor"]).expect("install parses");
        match cli.action {
            NodeAction::Install {
                yes, show_prompts, ..
            } => {
                assert!(!yes, "--yes defaults to false");
                assert!(!show_prompts, "--show-prompts defaults to false");
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

    #[test]
    fn install_report_names_the_source() {
        let lines = install_report_lines(&serde_json::json!({
            "name": "auditor",
            "kind": "agent",
            "source_url": "https://graphs.univerlab.org/i/draft/abcdefghijklmnopqrstuvwxyz",
            "source": "draft",
            "presets_written": [],
        }));
        assert!(
            lines.iter().any(|line| line == "source: draft"),
            "{lines:?}"
        );
    }

    /// RAII guard: points `CANOPY_HOME_OVERRIDE` at `path` for the test
    /// body, restoring the previous value on drop. Safe under `cargo
    /// nextest` (one process per test) but would race under `cargo test`.
    struct OverrideHome {
        prev: Option<std::ffi::OsString>,
    }

    impl OverrideHome {
        fn set(path: &std::path::Path) -> Self {
            let prev = std::env::var_os("CANOPY_HOME_OVERRIDE");
            unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", path) };
            OverrideHome { prev }
        }
    }

    impl Drop for OverrideHome {
        fn drop(&mut self) {
            match &self.prev {
                Some(value) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", value) },
                None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
            }
        }
    }

    /// A temp home whose `config.toml` points the catalogue at the stub,
    /// with empty nodes and prompts dirs.
    fn install_home(
        catalogue_url: &str,
    ) -> (
        tempfile::TempDir,
        OverrideHome,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let home = tempfile::tempdir().unwrap();
        let canopy = home.path().join(".canopy");
        let nodes = canopy.join("nodes");
        let prompts = canopy.join("prompts");
        std::fs::create_dir_all(&nodes).unwrap();
        std::fs::create_dir_all(&prompts).unwrap();
        std::fs::write(
            canopy.join("config.toml"),
            format!("[graphs]\ncatalogue_url = \"{catalogue_url}\"\n"),
        )
        .unwrap();
        let guard = OverrideHome::set(home.path());
        (home, guard, nodes, prompts)
    }

    fn auditor_doc() -> serde_json::Value {
        serde_json::json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this", "timeout_minutes": 10},
            "preset": {"name": "auditor-preset", "content": "audit well"},
        })
    }

    /// Without a TTY and without `--yes`: the summary was fetched (the
    /// stub was hit), the refusal names `--yes`, and nothing was written.
    #[test]
    fn without_a_tty_and_without_yes_install_refuses_and_writes_nothing() {
        use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
        let stub = StubCatalogue::start(
            [(
                "/i/node/auditor".to_string(),
                StubRoute::ok(&auditor_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let (_home, _guard, nodes, prompts) = install_home(&stub.base_url());

        let error = run_install(
            "auditor",
            false,
            false,
            false,
            false,
            false,
            &mut "".as_bytes(),
        )
        .expect_err("non-TTY without --yes must refuse");
        assert!(
            error.to_string().contains(
                "refusing to install without confirmation: re-run with --yes after \
                     reviewing the commands above"
            ),
            "{error}"
        );
        assert_eq!(stub.hits().len(), 1, "the review was fetched first");
        assert!(
            !nodes.join("auditor.json").exists(),
            "a refused install writes no node document"
        );
        assert!(
            !prompts.join("auditor-preset.md").exists(),
            "a refused install writes no preset"
        );
    }

    /// `--yes` prints the same summary and installs without asking.
    #[test]
    fn yes_installs_the_node_document_after_printing_the_summary() {
        use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
        let stub = StubCatalogue::start(
            [(
                "/i/node/auditor".to_string(),
                StubRoute::ok(&auditor_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let (_home, _guard, nodes, prompts) = install_home(&stub.base_url());

        run_install(
            "auditor",
            false,
            true,
            false,
            false,
            false,
            &mut "".as_bytes(),
        )
        .expect("--yes installs without a TTY");
        assert!(
            nodes.join("auditor.json").exists(),
            "the document was written after --yes"
        );
        assert_eq!(
            std::fs::read_to_string(prompts.join("auditor-preset.md")).unwrap(),
            "audit well"
        );
    }

    /// A valid draft id: 26 lowercase base32 characters.
    const DRAFT_ID: &str = "abcdefghijklmnopqrstuvwxyz";

    fn draft_reference() -> String {
        format!("draft:{DRAFT_ID}")
    }

    /// Refusing a draft says it is gone and names a new link: the review
    /// fetch already consumed it. Nothing is written.
    #[test]
    fn refusing_a_draft_names_a_new_link_and_writes_nothing() {
        use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
        let stub = StubCatalogue::start(
            [(
                format!("/i/draft/{DRAFT_ID}"),
                StubRoute::ok(&auditor_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let (_home, _guard, nodes, prompts) = install_home(&stub.base_url());

        let error = run_install(
            &draft_reference(),
            false,
            false,
            false,
            false,
            false,
            &mut "".as_bytes(),
        )
        .expect_err("non-TTY without --yes must refuse");
        assert!(error.to_string().contains("gone"), "{error}");
        assert!(
            error
                .to_string()
                .contains("copy a new link from the editor"),
            "{error}"
        );
        assert_eq!(stub.hits().len(), 1, "the review was fetched first");
        assert!(
            !nodes.join("auditor.json").exists(),
            "a refused install writes no node document"
        );
        assert!(
            !prompts.join("auditor-preset.md").exists(),
            "a refused install writes no preset"
        );
    }

    /// `--yes` installs a draft in its single fetch.
    #[test]
    fn yes_installs_a_draft() {
        use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
        let stub = StubCatalogue::start(
            [(
                format!("/i/draft/{DRAFT_ID}"),
                StubRoute::ok(&auditor_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let (_home, _guard, nodes, _prompts) = install_home(&stub.base_url());

        run_install(
            &draft_reference(),
            false,
            true,
            false,
            false,
            false,
            &mut "".as_bytes(),
        )
        .expect("--yes installs a draft");
        assert!(
            nodes.join("auditor.json").exists(),
            "the draft document was written"
        );
        assert_eq!(stub.hits().len(), 1, "one fetch installs a draft");
    }

    /// A TTY answering `y` installs; answering empty refuses.
    #[test]
    fn a_tty_answering_y_installs_but_empty_refuses() {
        use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
        let stub = StubCatalogue::start(
            [(
                "/i/node/auditor".to_string(),
                StubRoute::ok(&auditor_doc().to_string()),
            )]
            .into_iter()
            .collect(),
        );
        let (_home, _guard, nodes, _prompts) = install_home(&stub.base_url());

        run_install(
            "auditor",
            false,
            false,
            false,
            false,
            true,
            &mut b"y\n".as_slice(),
        )
        .expect("y installs");
        assert!(nodes.join("auditor.json").exists());

        let declined = run_install(
            "auditor",
            false,
            false,
            false,
            false,
            true,
            &mut b"\n".as_slice(),
        )
        .expect_err("an empty answer refuses");
        assert!(declined.to_string().contains("'auditor'"), "{declined}");
    }
}
