use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::setup_module::Platform;

use super::setup;
use super::PlatformConfigs;

pub fn collect_all_platform_configs(home: &Path, detected: &[&Platform]) -> PlatformConfigs {
    detected
        .iter()
        .map(|platform| (platform.name.clone(), read_platform_config(home, platform)))
        .collect()
}

pub(super) fn read_platform_config(
    home: &Path,
    platform: &Platform,
) -> BTreeMap<String, serde_json::Value> {
    let config_path = resolve_platform_config_path(home, platform);
    if !config_path.exists() {
        return BTreeMap::new();
    }

    match crate::config::McpConfigRegistry::extract_from_platform(
        &platform.name,
        &config_path,
        &platform.mcp_servers_key,
    ) {
        Ok(config) => config
            .servers
            .into_iter()
            .map(|server| (server.name, server.config))
            .collect(),
        Err(error) => {
            eprintln!(
                "  \x1b[33m⚠\x1b[0m Warning: could not read {} config: {}",
                platform.name, error
            );
            BTreeMap::new()
        }
    }
}

pub fn existing_server_names(
    all_configs: &PlatformConfigs,
    platform_name: &str,
) -> BTreeSet<String> {
    all_configs
        .get(platform_name)
        .map(|servers| servers.keys().cloned().collect())
        .unwrap_or_default()
}

/// Resolve the config file path for a platform (mirrors `setup::resolve_config_path`).
pub fn resolve_platform_config_path(home: &Path, platform: &Platform) -> PathBuf {
    let primary = home.join(&platform.config_path);
    if primary.exists() {
        return primary;
    }

    let ext = primary
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("");
    let alternate = match ext {
        "jsonc" => primary.with_extension("json"),
        "json" => primary.with_extension("jsonc"),
        _ => return primary,
    };

    if alternate.exists() {
        return alternate;
    }

    primary
}

/// Write a server config entry to a platform's config file.
pub fn apply_server_to_platform(
    platform: &Platform,
    config_path: &Path,
    server_name: &str,
    config: &serde_json::Value,
) -> Result<bool> {
    ensure_platform_config_exists(platform, config_path)?;

    if is_toml_platform(platform) {
        return upsert_toml_server(platform, config_path, server_name, config);
    }

    upsert_json_server(platform, config_path, config, server_name)
}

pub(super) fn ensure_platform_config_exists(platform: &Platform, config_path: &Path) -> Result<()> {
    if config_path.exists() {
        return Ok(());
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(config_path, initial_platform_config(platform))?;
    Ok(())
}

pub fn initial_platform_config(platform: &Platform) -> String {
    if is_toml_platform(platform) {
        return String::new();
    }

    format!("{{\"{}\": {{}}}}\n", primary_servers_key(platform))
}

pub fn is_toml_platform(platform: &Platform) -> bool {
    platform.config_format.as_deref() == Some("toml")
}

pub fn primary_servers_key(platform: &Platform) -> &str {
    platform
        .mcp_servers_key
        .first()
        .map(|key| key.as_str())
        .unwrap_or("mcpServers")
}

pub(super) fn upsert_toml_server(
    platform: &Platform,
    config_path: &Path,
    server_name: &str,
    config: &serde_json::Value,
) -> Result<bool> {
    if platform.toml_array_format {
        return setup::upsert_toml_array_pub(
            config_path,
            &platform.mcp_servers_key.join("."),
            server_name,
            config,
        );
    }

    setup::upsert_toml_key_pub(
        config_path,
        primary_servers_key(platform),
        server_name,
        config,
    )
}

pub(super) fn upsert_json_server(
    platform: &Platform,
    config_path: &Path,
    config: &serde_json::Value,
    server_name: &str,
) -> Result<bool> {
    let mut key_refs: Vec<&str> = platform
        .mcp_servers_key
        .iter()
        .map(|key| key.as_str())
        .collect();
    key_refs.push(server_name);
    setup::upsert_json_key_pub(config_path, &key_refs, config)
}

/// Remove a server entry from a platform config file.
pub fn remove_server_from_platform(
    platform: &Platform,
    config_path: &Path,
    server_name: &str,
) -> Result<bool> {
    if !config_path.exists() {
        return Ok(false);
    }

    if is_toml_platform(platform) {
        return setup::remove_toml_server_pub(platform, config_path, server_name);
    }

    setup::remove_json_key_pub(config_path, primary_servers_key(platform), server_name)
}

// ── Remove operation helpers ─────────────────────────────────────────────────

pub fn target_platforms_for_server<'a>(
    detected: &'a [&Platform],
    all_configs: &PlatformConfigs,
    server_name: &str,
) -> Vec<&'a Platform> {
    detected
        .iter()
        .copied()
        .filter(|platform| {
            all_configs
                .get(&platform.name)
                .is_some_and(|servers| servers.contains_key(server_name))
        })
        .collect()
}

pub fn remove_server_from_platforms(
    home: &Path,
    target_platforms: &[&Platform],
    server_name: &str,
) -> (usize, usize) {
    let mut ok = 0usize;
    let mut fail = 0usize;

    for platform in target_platforms {
        let config_path = resolve_platform_config_path(home, platform);
        match remove_server_from_platform(platform, &config_path, server_name) {
            Ok(true) => {
                println!("    \x1b[32m✓\x1b[0m {}", platform.name);
                ok += 1;
            }
            Ok(false) => println!(
                "    \x1b[33m–\x1b[0m {} (not found, skipped)",
                platform.name
            ),
            Err(error) => {
                println!("    \x1b[31m✗\x1b[0m {}: {error}", platform.name);
                fail += 1;
            }
        }
    }

    (ok, fail)
}

// ── Add operation helpers ────────────────────────────────────────────────────

pub fn add_server_to_platforms(
    home: &Path,
    detected: &[&Platform],
    name: &str,
    config: &serde_json::Value,
) -> (usize, usize) {
    let mut ok = 0usize;
    let mut fail = 0usize;

    for platform in detected {
        let config_path = resolve_platform_config_path(home, platform);
        let adapted = setup::adapt_config(config, platform, name);

        match apply_server_to_platform(platform, &config_path, name, &adapted) {
            Ok(_) => {
                println!("    \x1b[32m✓\x1b[0m {}", platform.name);
                ok += 1;
            }
            Err(error) => {
                println!("    \x1b[31m✗\x1b[0m {}: {error}", platform.name);
                fail += 1;
            }
        }
    }

    (ok, fail)
}
