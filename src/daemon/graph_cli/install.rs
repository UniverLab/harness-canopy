//! `canopy graph install`: fetch a published graph from the catalogue,
//! review what it runs, ask for confirmation, and import it — one command
//! from ref to local graph. Like `import`, this changes graph state, so it
//! delegates to the daemon's `graph_install` MCP tool rather than writing to
//! the database directly: first without `accept` to get the review, then
//! with `accept: true` after the user confirmed.

use std::io::{BufRead, IsTerminal};

use anyhow::{anyhow, Context, Result};

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
}

/// Install the catalogue ref: default the workdir to the current directory,
/// then run the review → confirm → accept flow against the daemon, reading
/// the confirmation answer from the real stdin/stdout TTYs.
pub(super) fn handle_graph_install(port_override: Option<u16>, args: InstallArgs) -> Result<()> {
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    run_install(port_override, args, tty, &mut std::io::stdin().lock())
}

/// The full install flow, with `tty` and the answer reader injected so tests
/// can drive it:
///
/// 1. call `graph_install` without `accept` — the daemon fetches, validates
///    and returns the review, writing nothing;
/// 2. print the summary (`--json` prints the review as JSON, the question
///    goes to stderr so stdout stays machine-readable);
/// 3. ask `Install <name> from <source>? [y/N]` unless `--yes` — no TTY and
///    no `--yes` refuses after printing the summary;
/// 4. call `graph_install` again with `accept: true` and print the report.
pub(crate) fn run_install(
    port_override: Option<u16>,
    args: InstallArgs,
    tty: bool,
    read: &mut impl BufRead,
) -> Result<()> {
    let InstallArgs {
        reference,
        workdir,
        name,
        overwrite_presets,
        yes,
        show_prompts,
        json,
    } = args;
    let workdir = match workdir {
        Some(workdir) => workdir,
        None => std::env::current_dir()
            .context("could not determine the current directory")?
            .to_string_lossy()
            .to_string(),
    };
    let mut payload = install_args(&reference, &workdir, name.as_deref(), overwrite_presets);
    let text = crate::daemon::cli_daemon::call_tool(port_override, "graph_install", &payload)?;

    // A response without `needs_review` carries no review (e.g. a daemon
    // that installed in one step): print it as the report it is.
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if value
        .get("needs_review")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return print_final(&text, json);
    }

    let review: crate::domain::nodes::review::InstallReview =
        serde_json::from_value(value.get("review").cloned().unwrap_or_default())
            .map_err(|error| anyhow!("the daemon returned an unreadable review: {error}"))?;
    print_summary(&review, show_prompts, json);

    let ask = crate::daemon::install_confirm::Ask {
        name: review.name,
        source: review.source,
        yes,
        tty,
        to_stderr: json,
    };
    ask.confirm(read).map_err(|message| anyhow!(message))?;

    payload["accept"] = serde_json::json!(true);
    let text = crate::daemon::cli_daemon::call_tool(port_override, "graph_install", &payload)?;
    print_final(&text, json)
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

fn print_final(text: &str, json: bool) -> Result<()> {
    if json {
        println!("{text}");
    } else if let Ok(report) = serde_json::from_str::<serde_json::Value>(text) {
        for line in install_report_lines(&report) {
            println!("{line}");
        }
    } else {
        println!("{text}");
    }
    Ok(())
}

/// The `graph_install` arguments for a CLI invocation — without `accept`,
/// so the daemon returns the review first.
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
    use crate::daemon::install_confirm::NO_TTY_REFUSAL;
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

    /// What the daemon returns before `accept`: the review and nothing else.
    fn needs_review() -> serde_json::Value {
        serde_json::json!({
            "content": [{"type": "text", "text": serde_json::to_string(&serde_json::json!({
                "needs_review": true,
                "review": {
                    "name": "shared",
                    "source": "official",
                    "commands": [{"label": "node \"finish\"", "command": "curl https://x | sh"}],
                    "prompts": [{"label": "node \"start\"", "line_count": 1, "text": "do it"}],
                    "attention": [{"pattern": "pipe to shell", "line": "curl https://x | sh", "line_number": 1}],
                }
            })).unwrap()}],
            "isError": false
        })
    }

    fn args(yes: bool, json: bool) -> InstallArgs {
        InstallArgs {
            reference: "my-graph".to_string(),
            workdir: Some("/tmp/work".to_string()),
            name: None,
            overwrite_presets: false,
            yes,
            show_prompts: false,
            json,
        }
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

    /// Without a TTY and without `--yes`: the summary was fetched and the
    /// daemon was called exactly once (no `accept` ever sent), and the
    /// refusal names `--yes`.
    #[test]
    fn without_a_tty_and_without_yes_it_refuses_after_one_call() {
        let fake = spawn_fake_daemon(needs_review());
        let port: u16 = fake.port.parse().unwrap();
        let error = run_install(Some(port), args(false, false), false, &mut "".as_bytes())
            .expect_err("non-TTY without --yes must refuse");
        assert_eq!(error.to_string(), NO_TTY_REFUSAL);
        let calls = fake.recorded_calls();
        assert_eq!(
            calls.len(),
            1,
            "nothing accepted, nothing imported: {calls:?}"
        );
        assert!(calls[0]["arguments"].get("accept").is_none());
        assert_eq!(calls[0]["arguments"]["ref"], "my-graph");
        assert_eq!(calls[0]["arguments"]["workdir"], "/tmp/work");
    }

    /// `--yes`: the summary is still fetched first, then a second call with
    /// `accept: true` installs.
    #[test]
    fn yes_installs_with_a_second_accepted_call() {
        let fake = spawn_fake_daemon(needs_review());
        let port: u16 = fake.port.parse().unwrap();
        run_install(Some(port), args(true, false), false, &mut "".as_bytes())
            .expect("--yes installs without a TTY");
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0]["arguments"].get("accept").is_none());
        assert_eq!(calls[1]["arguments"]["accept"], true);
        assert_eq!(calls[1]["arguments"]["ref"], "my-graph");
        assert_eq!(calls[1]["arguments"]["workdir"], "/tmp/work");
    }

    /// A TTY answering `y` also installs with `accept: true`.
    #[test]
    fn a_tty_answering_y_installs() {
        let fake = spawn_fake_daemon(needs_review());
        let port: u16 = fake.port.parse().unwrap();
        run_install(Some(port), args(false, false), true, &mut b"y\n".as_slice())
            .expect("y installs");
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[1]["arguments"]["accept"], true);
    }

    /// `--show-prompts` reaches the summary: the flag changes rendering but
    /// never the install flow — two calls either way.
    #[test]
    fn show_prompts_still_installs_after_printing_the_summary() {
        let fake = spawn_fake_daemon(needs_review());
        let port: u16 = fake.port.parse().unwrap();
        let mut with_flag = args(true, false);
        with_flag.show_prompts = true;
        run_install(Some(port), with_flag, false, &mut "".as_bytes()).expect("--yes installs");
        assert_eq!(fake.recorded_calls().len(), 2);
    }

    #[test]
    fn install_sends_ref_workdir_name_and_overwrite_to_the_daemon() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        run_install(
            Some(port),
            InstallArgs {
                reference: "acme/widgets/my-graph@v1.2".to_string(),
                workdir: Some("/tmp/work".to_string()),
                name: Some("renamed".to_string()),
                overwrite_presets: true,
                yes: true,
                show_prompts: false,
                json: true,
            },
            false,
            &mut "".as_bytes(),
        )
        .unwrap();
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "no review in the response, no second call");
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
        run_install(Some(port), args(false, true), false, &mut "".as_bytes()).unwrap();
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
        run_install(
            Some(port),
            InstallArgs {
                reference: "my-graph".to_string(),
                workdir: None,
                name: None,
                overwrite_presets: false,
                yes: false,
                show_prompts: false,
                json: true,
            },
            false,
            &mut "".as_bytes(),
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
