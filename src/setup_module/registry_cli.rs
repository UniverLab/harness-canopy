//! `canopy registry` — apply the platform registry to `config.toml` without
//! the TUI. Registry fixes used to ship only through the 24-hour background
//! refresh inside the TUI's startup path, which left operators hand-editing
//! `config.toml`; this command (and the `harness refresh_registry` action) is the
//! missing on-demand surface. It shares the exact merge path the TUI uses
//! ([`super::registry_fetch::refresh_registry_inner`]), rewrites only
//! `config.toml` and the baseline file, and never starts a TUI, wizard or
//! daemon.

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
}

pub(crate) fn handle_registry_action(action: RegistryAction) -> anyhow::Result<()> {
    match action {
        RegistryAction::Refresh {
            local_registry,
            json,
        } => handle_registry_refresh(local_registry, json),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
