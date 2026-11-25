//! `canopy registry` — apply the platform registry to `config.toml` without
//! the TUI. Registry fixes used to ship only through the 24-hour background
//! refresh inside the TUI's startup path, which left operators hand-editing
//! `config.toml`; this command (and the `harness refresh_registry` action) is the
//! missing on-demand surface. It shares the exact merge path the TUI uses
//! ([`super::registry_fetch::refresh_registry_inner`]), rewrites only
//! `config.toml` and the baseline file, and never starts a TUI, wizard or
//! daemon.
//!
//! Executable registry changes (command lines, flags, argv templates) are
//! never applied by a refresh: they are staged in `registry-pending.json`
//! and take effect only through `canopy registry apply`, which is a human
//! approval step with no MCP equivalent.

use clap::Subcommand;
use std::path::PathBuf;

#[derive(Subcommand)]
pub(crate) enum RegistryAction {
    /// Fetch the platform registry and apply it now, ignoring the 24-hour interval.
    Refresh {
        /// Apply a local registry directory instead of fetching from GitHub.
        /// Same as `canopy setup --local-registry`; useful for testing
        /// registry changes before publishing.
        #[arg(long = "local-registry", value_name = "PATH")]
        local_registry: Option<PathBuf>,
        /// Print the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Show the registry changes waiting for approval.
    Pending {
        /// Print the staged file content as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Approve staged registry changes and write them to `config.toml`.
    Apply {
        /// Only apply the changes for this CLI; default is all of them.
        #[arg(value_name = "CLI")]
        cli: Option<String>,
        /// Approve without prompting. Without it, approval needs a terminal.
        #[arg(long)]
        yes: bool,
    },
    /// Drop staged registry changes without applying them. The next refresh
    /// stages them again while the registry still differs.
    Discard {
        /// Only discard the changes for this CLI; default is all of them.
        #[arg(value_name = "CLI")]
        cli: Option<String>,
    },
}

pub(crate) fn handle_registry_action(action: RegistryAction) -> anyhow::Result<()> {
    match action {
        RegistryAction::Refresh {
            local_registry,
            json,
        } => handle_registry_refresh(local_registry, json),
        RegistryAction::Pending { json } => {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
            println!("{}", run_pending(&home, json)?);
            Ok(())
        }
        RegistryAction::Apply { cli, yes } => handle_registry_apply(cli.as_deref(), yes),
        RegistryAction::Discard { cli } => {
            let home = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
            let removed = discard_pending(&home, cli.as_deref())?;
            println!("discarded {removed} change(s)");
            Ok(())
        }
    }
}

/// Run one immediate registry refresh and print the per-platform report.
fn handle_registry_refresh(local_registry: Option<PathBuf>, json: bool) -> anyhow::Result<()> {
    if let Some(path) = local_registry {
        super::registry_fetch::set_local_registry(path);
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let report = run_refresh(&home, super::registry_fetch::fetch_registry)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!("{}", report.render_text());
    }
    Ok(())
}

/// The command's one refresh, minus `dirs::home_dir()` so tests can drive it
/// against a temp home. Bails before any fetch when there is no `config.toml`
/// yet — a fresh machine has nothing for the registry merge to work on, and
/// `canopy setup` is the thing that creates it.
fn run_refresh(
    home: &std::path::Path,
    fetch: impl FnOnce() -> anyhow::Result<crate::setup_module::models::RegistryRaw>,
) -> anyhow::Result<crate::setup_module::registry_fetch::RegistryRefreshReport> {
    let config_path = home.join(".canopy").join("config.toml");
    anyhow::ensure!(
        config_path.exists(),
        "No config.toml at {} — run `canopy setup` first",
        config_path.display()
    );
    super::registry_fetch::refresh_registry_inner(home, fetch)
}

/// What `pending` prints: the staged file content as JSON, or
/// `{"changes":[]}` when nothing is staged.
fn run_pending(home: &std::path::Path, json: bool) -> anyhow::Result<String> {
    use super::registry_pending::{format_pending, load_pending};
    if json {
        let path = home
            .join(".canopy")
            .join(super::registry_pending::PENDING_FILE);
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(content),
            Err(_) => Ok("{\"changes\":[]}".to_string()),
        }
    } else {
        let staged = load_pending(&home.join(".canopy"));
        if staged.is_empty() {
            Ok("no registry changes waiting for approval".to_string())
        } else {
            Ok(staged
                .iter()
                .map(format_pending)
                .collect::<Vec<_>>()
                .join("\n"))
        }
    }
}

/// Pick the staged changes `apply`/`discard` operate on. Errors when there
/// is nothing to do, naming the CLI when it simply has nothing staged.
fn select_pending(
    home: &std::path::Path,
    cli: Option<&str>,
) -> anyhow::Result<Vec<super::registry_pending::PendingChange>> {
    use super::registry_pending::load_pending;
    let staged = load_pending(&home.join(".canopy"));
    if staged.is_empty() {
        anyhow::bail!("no registry changes waiting for approval");
    }
    match cli {
        None => Ok(staged),
        Some(name) => {
            let selected: Vec<_> = staged.into_iter().filter(|c| c.cli == name).collect();
            if selected.is_empty() {
                anyhow::bail!("no pending change for {name}");
            }
            Ok(selected)
        }
    }
}

/// Refuse a non-interactive approval: without `--yes` the operator must be
/// at a terminal answering the prompt. Split out so tests drive the real
/// gate (`std::io::stdin()` is never a terminal under test).
fn ensure_terminal_or_yes(yes: bool) -> anyhow::Result<()> {
    use std::io::IsTerminal as _;
    if !yes && !std::io::stdin().is_terminal() {
        anyhow::bail!("canopy registry apply needs a terminal; pass --yes to approve without one");
    }
    Ok(())
}

/// True only for an explicit yes: anything else aborts the approval.
fn confirms(input: &str) -> bool {
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// What one `apply` run did: lines for skipped entries plus the count of
/// fields actually written.
struct ApplyOutcome {
    skipped: Vec<String>,
    applied: usize,
}

/// Write the selected changes into the `config.toml` CLI entries and the
/// baseline sidecar, and drop them from the staged file. A change whose CLI
/// is no longer in `config.toml` is skipped and dropped.
fn apply_pending(
    home: &std::path::Path,
    selected: &[super::registry_pending::PendingChange],
) -> anyhow::Result<ApplyOutcome> {
    use super::registry_pending::{load_pending, save_pending, set_cli_field};
    let canopy_dir = home.join(".canopy");
    let mut config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    let mut baseline = crate::domain::registry_baseline::RegistryBaseline::load(&canopy_dir);

    let mut applied = 0;
    let mut skipped = Vec::new();
    for change in selected {
        let Some(entry) = config.clis.iter_mut().find(|c| c.name == change.cli) else {
            skipped.push(format!("{}: not in config, skipped", change.cli));
            continue;
        };
        if set_cli_field(entry, &change.field, change.new.clone()) {
            applied += 1;
        }
        if let Some(baseline) = baseline.as_mut() {
            if let Some(entry) = baseline.clis.iter_mut().find(|c| c.name == change.cli) {
                set_cli_field(entry, &change.field, change.new.clone());
            }
        }
    }
    crate::domain::settings::writer::write_clis(&canopy_dir, &config.clis)?;
    if let Some(baseline) = baseline {
        baseline.save(&canopy_dir)?;
    }

    let selected_set: Vec<(&str, &str)> = selected
        .iter()
        .map(|c| (c.cli.as_str(), c.field.as_str()))
        .collect();
    let remaining: Vec<_> = load_pending(&canopy_dir)
        .into_iter()
        .filter(|c| !selected_set.contains(&(c.cli.as_str(), c.field.as_str())))
        .collect();
    save_pending(&canopy_dir, &remaining)?;

    Ok(ApplyOutcome { skipped, applied })
}

/// Drop the selected staged changes without applying them.
fn discard_pending(home: &std::path::Path, cli: Option<&str>) -> anyhow::Result<usize> {
    use super::registry_pending::{load_pending, save_pending};
    let canopy_dir = home.join(".canopy");
    let staged = load_pending(&canopy_dir);
    let (removed, remaining): (Vec<_>, Vec<_>) = staged
        .into_iter()
        .partition(|c| cli.is_none_or(|name| c.cli == name));
    save_pending(&canopy_dir, &remaining)?;
    Ok(removed.len())
}

fn handle_registry_apply(cli: Option<&str>, yes: bool) -> anyhow::Result<()> {
    use super::registry_pending::format_pending;
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let selected = select_pending(&home, cli)?;
    for change in &selected {
        println!("{}", format_pending(change));
    }
    ensure_terminal_or_yes(yes)?;
    if !yes {
        use std::io::Write as _;
        print!("Apply {} change(s)? [y/N] ", selected.len());
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !confirms(&input) {
            println!("nothing applied");
            return Ok(());
        }
    }
    let outcome = apply_pending(&home, &selected)?;
    for line in &outcome.skipped {
        println!("{line}");
    }
    println!("applied {} change(s)", outcome.applied);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_cli(headless_mode: &str) -> crate::domain::cli_config::CliConfig {
        crate::domain::cli_config::CliConfig {
            name: "testcli".to_string(),
            binary: "ls".to_string(),
            headless_mode: headless_mode.to_string(),
            paste_submit_presses: 1,
            ..Default::default()
        }
    }

    fn staged(headless_new: &str) -> crate::setup_module::registry_pending::PendingChange {
        crate::setup_module::registry_pending::PendingChange {
            cli: "testcli".to_string(),
            field: "headless_mode".to_string(),
            old: serde_json::json!("--old"),
            new: serde_json::json!(headless_new),
            fetched_at: "t".to_string(),
        }
    }

    /// A temp home with a config entry, a matching baseline entry and one
    /// staged `headless_mode` change.
    fn staged_home() -> TempDir {
        let home = TempDir::new().unwrap();
        let canopy_dir = home.path().join(".canopy");
        let cli = test_cli("--old");
        crate::domain::settings::writer::write_clis(&canopy_dir, std::slice::from_ref(&cli))
            .unwrap();
        crate::domain::registry_baseline::RegistryBaseline {
            clis: vec![cli],
            ..Default::default()
        }
        .save(&canopy_dir)
        .unwrap();
        crate::setup_module::registry_pending::save_pending(&canopy_dir, &[staged("--new")])
            .unwrap();
        home
    }

    /// The command refuses to run — and must not fetch — when the machine has
    /// no `config.toml` yet. The fetch closure panics, so a test failure here
    /// also proves it never ran.
    #[test]
    fn refresh_bails_without_config_toml_and_never_fetches() {
        let home = TempDir::new().unwrap();
        let err = run_refresh(home.path(), || {
            panic!("fetch must not run before the config.toml check")
        })
        .unwrap_err();
        assert!(err.to_string().contains("run `canopy setup`"), "{err}");
    }

    #[test]
    fn pending_text_and_json() {
        let home = staged_home();
        let text = run_pending(home.path(), false).unwrap();
        assert_eq!(text, "testcli.headless_mode: \"--old\" -> \"--new\"");

        let json = run_pending(home.path(), true).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["changes"].as_array().unwrap().len(), 1);

        let empty = TempDir::new().unwrap();
        assert_eq!(
            run_pending(empty.path(), false).unwrap(),
            "no registry changes waiting for approval"
        );
        assert_eq!(run_pending(empty.path(), true).unwrap(), "{\"changes\":[]}");
    }

    #[test]
    fn apply_yes_writes_config_and_baseline_and_empties() {
        let home = staged_home();
        let selected = select_pending(home.path(), None).unwrap();
        assert_eq!(selected.len(), 1);
        let outcome = apply_pending(home.path(), &selected).unwrap();
        assert_eq!(outcome.applied, 1);
        assert!(outcome.skipped.is_empty());

        let config = crate::domain::canopy_config::CanopyConfig::load(&home.path().join(".canopy"));
        assert_eq!(config.get_cli("testcli").unwrap().headless_mode, "--new");
        let baseline =
            crate::domain::registry_baseline::RegistryBaseline::load(&home.path().join(".canopy"))
                .unwrap();
        assert_eq!(baseline.get("testcli").unwrap().headless_mode, "--new");
        assert!(
            crate::setup_module::registry_pending::load_pending(&home.path().join(".canopy"))
                .is_empty()
        );
    }

    #[test]
    fn apply_for_unknown_cli_fails_while_others_wait() {
        let home = staged_home();
        let err = select_pending(home.path(), Some("ghost")).unwrap_err();
        assert_eq!(err.to_string(), "no pending change for ghost");
        // Nothing was touched by the failed selection.
        assert_eq!(
            crate::setup_module::registry_pending::load_pending(&home.path().join(".canopy")).len(),
            1
        );
    }

    #[test]
    fn select_with_nothing_staged_reports_it() {
        let home = TempDir::new().unwrap();
        let err = select_pending(home.path(), None).unwrap_err();
        assert_eq!(err.to_string(), "no registry changes waiting for approval");
        // A CLI filter on an empty staged file reports the same: there is
        // nothing staged at all, so no CLI is "unknown with others pending".
        let err = select_pending(home.path(), Some("testcli")).unwrap_err();
        assert_eq!(err.to_string(), "no registry changes waiting for approval");
    }

    #[test]
    fn apply_without_yes_and_without_terminal_fails_unchanged() {
        // `stdin` is never a terminal under test, so the real gate fires.
        let err = ensure_terminal_or_yes(false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "canopy registry apply needs a terminal; pass --yes to approve without one"
        );
        assert!(ensure_terminal_or_yes(true).is_ok());

        // The gate runs before anything is written: staging is untouched.
        let home = staged_home();
        assert!(ensure_terminal_or_yes(false).is_err());
        assert_eq!(
            crate::setup_module::registry_pending::load_pending(&home.path().join(".canopy")).len(),
            1
        );
        let config = crate::domain::canopy_config::CanopyConfig::load(&home.path().join(".canopy"));
        assert_eq!(config.get_cli("testcli").unwrap().headless_mode, "--old");
    }

    #[test]
    fn confirm_accepts_only_explicit_yes() {
        assert!(confirms("y\n"));
        assert!(confirms("YES"));
        assert!(!confirms(""));
        assert!(!confirms("n"));
        assert!(!confirms("yeah"));
    }

    #[test]
    fn discard_empties_and_missing_cli_is_skipped_on_apply() {
        let home = staged_home();
        assert_eq!(discard_pending(home.path(), Some("ghost")).unwrap(), 0);
        assert_eq!(discard_pending(home.path(), None).unwrap(), 1);
        assert!(
            crate::setup_module::registry_pending::load_pending(&home.path().join(".canopy"))
                .is_empty()
        );

        // A staged change for a CLI that left `config.toml` is dropped, not applied.
        crate::setup_module::registry_pending::save_pending(
            &home.path().join(".canopy"),
            &[
                staged("--new"),
                crate::setup_module::registry_pending::PendingChange {
                    cli: "ghost".to_string(),
                    field: "headless_mode".to_string(),
                    old: serde_json::json!("--old"),
                    new: serde_json::json!("--new"),
                    fetched_at: "t".to_string(),
                },
            ],
        )
        .unwrap();
        let mut config =
            crate::domain::canopy_config::CanopyConfig::load(&home.path().join(".canopy"));
        config.clis.retain(|c| c.name != "testcli");
        crate::domain::settings::writer::write_clis(&home.path().join(".canopy"), &config.clis)
            .unwrap();
        let selected = select_pending(home.path(), None).unwrap();
        assert_eq!(selected.len(), 2);
        let outcome = apply_pending(home.path(), &selected).unwrap();
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.skipped.len(), 2);
        assert!(outcome
            .skipped
            .contains(&"ghost: not in config, skipped".to_string()));
        assert!(outcome
            .skipped
            .contains(&"testcli: not in config, skipped".to_string()));
        assert!(
            crate::setup_module::registry_pending::load_pending(&home.path().join(".canopy"))
                .is_empty()
        );
    }
}
