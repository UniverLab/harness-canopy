use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;

use crate::setup_module::Platform;

use super::confirm;
use super::platform_config::{
    collect_all_platform_configs, remove_server_from_platforms, target_platforms_for_server,
};
use super::show_updated_table;
use super::PlatformConfigs;

/// Show every unique MCP server found in any platform and remove the chosen
/// one from every platform where it exists.
pub fn run_remove(home: &Path, detected: &[&Platform]) -> Result<()> {
    println!();
    println!("  \x1b[1mRemove MCP Server\x1b[0m");
    println!("  ─────────────────────────────────────────────");

    let all_configs = collect_all_platform_configs(home, detected);
    let Some(selected) = prompt_server_to_remove(&all_configs)? else {
        return Ok(());
    };

    let target_platforms = target_platforms_for_server(detected, &all_configs, &selected);
    if !confirm(&format!(
        "Will remove \x1b[1m{selected}\x1b[0m from: {}. Proceed?",
        target_platforms
            .iter()
            .map(|platform| platform.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))? {
        println!("  Cancelled.");
        return Ok(());
    }

    let (ok, fail) = remove_server_from_platforms(home, &target_platforms, &selected);

    println!();
    if fail == 0 {
        println!("  \x1b[32m✓\x1b[0m '{selected}' removed from {ok} platform(s).");
    } else {
        println!("  \x1b[33m⚠\x1b[0m Removed from {ok}, failed on {fail}.");
    }

    println!();
    show_updated_table(home, detected);
    Ok(())
}

pub fn collect_all_server_names(all_configs: &PlatformConfigs) -> BTreeSet<String> {
    all_configs
        .values()
        .flat_map(|servers| servers.keys().cloned())
        .collect()
}

pub(super) fn prompt_server_to_remove(all_configs: &PlatformConfigs) -> Result<Option<String>> {
    let all_names = collect_all_server_names(all_configs);
    if all_names.is_empty() {
        println!("  \x1b[33m⚠\x1b[0m  No MCP servers found.");
        return Ok(None);
    }

    let choices: Vec<String> = all_names.into_iter().collect();
    let selected = inquire::Select::new("Select server to remove:", choices)
        .with_help_message("Enter to confirm | Esc to cancel")
        .prompt()
        .map_err(super::cancelled_prompt)?;

    Ok(Some(selected))
}
