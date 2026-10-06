use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

use crate::setup_module::Platform;

use super::platform_config::{
    apply_server_to_platform, collect_all_platform_configs, existing_server_names,
    resolve_platform_config_path,
};
use super::render::print_mcp_table;
use super::setup;
use super::{confirm, show_updated_table};
use super::{MissingServers, PlatformConfigs, UnifiedServers};

/// Collect all MCP servers from every platform then replicate each missing one
/// to every platform that does not yet have it.
pub fn run_sync(home: &Path, detected: &[&Platform]) -> Result<()> {
    print_sync_header();

    let all_configs = collect_all_platform_configs(home, detected);
    let unified = build_unified_servers(&all_configs);
    if unified.is_empty() {
        println!("  \x1b[33m⚠\x1b[0m  No MCP servers found in any platform config.");
        return Ok(());
    }

    println!();
    print_mcp_table(detected, &all_configs);

    let missing = collect_missing_servers(detected, &all_configs, &unified);
    if missing.is_empty() {
        println!("  \x1b[32m✓\x1b[0m All platforms already in sync.");
        return Ok(());
    }

    let missing_count = missing.len();
    if !confirm(&format!(
        "\x1b[33m{missing_count}\x1b[0m server(s) to replicate. Proceed?"
    ))? {
        println!("  Cancelled.");
        return Ok(());
    }

    let (applied, errors) = apply_sync_to_platforms(home, detected, &all_configs, &unified);
    print_sync_summary(applied, errors);

    println!();
    show_updated_table(home, detected);
    Ok(())
}

pub(super) fn print_sync_header() {
    println!();
    println!("  \x1b[1mGlobal MCP Sync\x1b[0m");
    println!("  ─────────────────────────────────────────────");
    println!("  Scanning platform configs…");
}

pub(super) fn build_unified_servers(all_configs: &PlatformConfigs) -> UnifiedServers<'_> {
    let mut unified = BTreeMap::new();

    for (platform, servers) in all_configs {
        for (name, config) in servers {
            unified
                .entry(name.clone())
                .or_insert_with(|| (config.clone(), platform.as_str()));
        }
    }

    unified
}

pub(super) fn collect_missing_servers(
    detected: &[&Platform],
    all_configs: &PlatformConfigs,
    unified: &UnifiedServers<'_>,
) -> MissingServers {
    let mut missing = Vec::new();

    for platform in detected {
        let existing = existing_server_names(all_configs, &platform.name);
        for name in unified.keys() {
            if !existing.contains(name) {
                missing.push((platform.name.clone(), name.clone()));
            }
        }
    }

    missing
}

pub(super) fn apply_sync_to_platforms(
    home: &Path,
    detected: &[&Platform],
    all_configs: &PlatformConfigs,
    unified: &UnifiedServers<'_>,
) -> (usize, usize) {
    let mut applied = 0usize;
    let mut errors = 0usize;

    for platform in detected {
        let config_path = resolve_platform_config_path(home, platform);
        let existing = existing_server_names(all_configs, &platform.name);

        for (server_name, (config, _)) in unified {
            if existing.contains(server_name) {
                continue;
            }

            let adapted = setup::adapt_config(config, platform, server_name);
            match apply_server_to_platform(platform, &config_path, server_name, &adapted) {
                Ok(_) => applied += 1,
                Err(error) => {
                    eprintln!(
                        "    \x1b[31m✗\x1b[0m {}/{}: {}",
                        platform.name, server_name, error
                    );
                    errors += 1;
                }
            }
        }
    }

    (applied, errors)
}

pub fn print_sync_summary(applied: usize, errors: usize) {
    println!();
    if errors == 0 {
        println!("  \x1b[32m✓\x1b[0m Sync complete — {applied} server(s) replicated.");
        return;
    }

    println!("  \x1b[33m⚠\x1b[0m Sync partial — {applied} synced, {errors} failed.");
}
