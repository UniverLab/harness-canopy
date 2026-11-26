use crate::setup_module::mcp_migrate::{migrate_canopy_entries, MigrationOutcome};
use crate::setup_module::models::{resolve_config_path, CanonicalServers, Platform};
use crate::setup_module::platform_adapter::{
    extract_all_mcp_configs, print_mcp_matrix, run_install_our_servers,
};
use crate::setup_module::wizard::WizardState;
use anyhow::Result;
use std::path::Path;

/// Run the interactive MCP setup/management step.
pub(crate) fn run_sync_step(
    wiz: &mut WizardState,
    home: &Path,
    selected: &[&Platform],
    canonical: &CanonicalServers,
) -> Result<Option<String>> {
    if selected.is_empty() {
        return Ok(None);
    }

    wiz.render()?;
    println!("  \x1b[1mMCP Manager\x1b[0m");
    println!("  ─────────────────────────────────────────────");
    println!();

    run_install_our_servers(home, selected, canonical)?;
    report_migration_results(home, selected);

    let all_configs = extract_all_mcp_configs(home, selected);
    if !all_configs.is_empty() {
        print_mcp_matrix(&all_configs);
    }

    Ok(Some("\x1b[32m✓\x1b[0m MCP servers updated".to_string()))
}

fn report_migration_results(home: &Path, selected: &[&Platform]) {
    for (platform, outcome) in selected.iter().zip(migrate_canopy_entries(home, selected)) {
        let path = resolve_config_path(home, &platform.config_path);
        match outcome {
            MigrationOutcome::Legacy => println!(
                "  {}: rewritten to canopy bridge (backup {}.canopy-bak)",
                platform.name,
                path.display()
            ),
            MigrationOutcome::Unparsable => println!(
                "  {}: needs attention: cannot parse {}",
                platform.name,
                path.display()
            ),
            MigrationOutcome::Current | MigrationOutcome::NoEntry => {}
        }
    }
}

/// Refresh the embedded `canopy` skill and remove skill copies earlier
/// versions installed (harness symlinks, Essential Pack directories, the
/// old MCP store). Runs best-effort so a network error never blocks setup
/// completion. `force_skills` also removes copies whose local content was
/// edited; without it, edited copies are kept.
pub(crate) fn run_essential_skills_step(
    home: &Path,
    selected: &[&Platform],
    force_skills: bool,
) -> String {
    // Write the embedded `canopy` skill into the configured harnesses
    // in-process (same function `canopy skill install` runs later).
    let owned: Vec<Platform> = selected.iter().map(|p| (*p).clone()).collect();
    for line in crate::skills_module::install_for_home(home, &owned, false) {
        tracing::info!("canopy skill: {line}");
    }

    match crate::skills_module::cleanup::GithubCatalogSource::new() {
        Ok(source) => {
            for line in crate::skills_module::cleanup::run_skills_cleanup(
                home,
                &owned,
                force_skills,
                &source,
            ) {
                println!("{line}");
            }
        }
        Err(error) => tracing::warn!("Skills cleanup skipped: {error}"),
    }

    "\x1b[32m✓\x1b[0m Skills: legacy copies cleaned".to_string()
}
