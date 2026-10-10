//! Interactive MCP management wizard — `canopy mcp`.
//!
//! Provides three operations:
//!  - **Sync**: scan all detected platforms and replicate MCPs found in any of
//!    them across every other platform (format-converted).
//!  - **Add**: ask how the server is described (paste README JSON, local
//!    stdio command, or remote http URL), preview the entries, then write
//!    them to every detected platform simultaneously.
//!  - **Remove**: show a unified server list and remove the chosen entry from
//!    every platform it appears in.

use anyhow::{Context, Result};
use inquire::{Select, Text};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use crate::setup_module::{self as setup, Platform};

// ── Sub-modules ───────────────────────────────────────────────────────────

mod add;
mod parse;
mod platform_config;
mod remove;
mod render;
mod sync;

// Re-exports used by this module's remaining flow functions.
use add::run_add;
use platform_config::collect_all_platform_configs;
use remove::run_remove;
use render::{clear_screen, print_mcp_banner, print_mcp_table, registry_source_label};
use sync::run_sync;

// ── Public entry point ─────────────────────────────────────────────────────

type PlatformConfigs = BTreeMap<String, BTreeMap<String, serde_json::Value>>;
type UnifiedServers<'a> = BTreeMap<String, (serde_json::Value, &'a str)>;
type MissingServers = Vec<(String, String)>;

#[derive(Clone, Copy)]
enum WizardAction {
    Sync,
    Add,
    Remove,
}

struct AddServerInput {
    name: String,
    server_type: String,
    transport: ServerTransport,
}

enum ServerTransport {
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
    Command {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Pasted(serde_json::Value),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddInputMode {
    Paste,
    LocalCommand,
    RemoteUrl,
}

/// Run the interactive `canopy mcp` wizard.
pub fn run_mcp_wizard() -> Result<()> {
    let home = dirs::home_dir().context("No home directory")?;

    clear_screen()?;
    print_mcp_banner();

    let registry = fetch_registry()?;
    let detected = detect_available_platforms(&registry.platforms);

    if detected.is_empty() {
        print_no_supported_platforms(&registry.platforms);
        return Ok(());
    }

    print_detected_platforms(&detected);
    let pre_configs = collect_all_platform_configs(&home, &detected);
    print_mcp_table(&detected, &pre_configs);

    let Some(action) = prompt_wizard_action()? else {
        return Ok(());
    };

    match action {
        WizardAction::Sync => run_sync(&home, &detected),
        WizardAction::Add => run_add(&home, &detected),
        WizardAction::Remove => run_remove(&home, &detected),
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn fetch_registry() -> Result<crate::setup_module::models::RegistryRaw> {
    print!("  Fetching platform registry… ");
    io::stdout().flush()?;
    let registry = setup::fetch_registry_raw().context("Failed to fetch registry")?;
    // CB65: show which registry the wizard actually used — a silent GitHub
    // fetch after `--local-registry` was once indistinguishable from success.
    let label =
        registry_source_label(setup::registry_fetch::local_registry_path().map(|p| p.as_path()));
    println!("\x1b[32m✓\x1b[0m ({})", label);
    Ok(registry)
}

fn detect_available_platforms(platforms: &[Platform]) -> Vec<&Platform> {
    platforms
        .iter()
        .filter(|platform| setup::is_platform_available(platform))
        .collect()
}

fn print_no_supported_platforms(platforms: &[Platform]) {
    println!(
        "  \x1b[33m⚠\x1b[0m  No supported platforms detected ({}). Install one and re-run.",
        platforms
            .iter()
            .map(|platform| platform.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

fn print_detected_platforms(detected: &[&Platform]) {
    println!(
        "  Platforms detected: \x1b[32m{}\x1b[0m",
        detected
            .iter()
            .map(|platform| platform.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();
}

fn prompt_wizard_action() -> Result<Option<WizardAction>> {
    let action = Select::new(
        "What would you like to do?",
        vec![
            "Sync — replicate MCPs across all platforms",
            "Add — register a new MCP server everywhere",
            "Remove — delete an MCP server from all platforms",
        ],
    )
    .with_help_message("↑↓ navigate | Enter select | Esc cancel")
    .prompt()
    .map_err(cancelled_prompt)?;

    Ok(match action {
        a if a.starts_with("Sync") => Some(WizardAction::Sync),
        a if a.starts_with("Add") => Some(WizardAction::Add),
        a if a.starts_with("Remove") => Some(WizardAction::Remove),
        _ => None,
    })
}

fn cancelled_prompt(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("Cancelled: {}", error)
}

fn prompt_trimmed_text(prompt: &str) -> Result<String> {
    Text::new(prompt)
        .prompt()
        .map(|value| value.trim().to_string())
        .map_err(cancelled_prompt)
}

fn validate_required_input(value: &str, error_message: &str) -> bool {
    if !value.is_empty() {
        return true;
    }

    println!("  \x1b[31m✗\x1b[0m {error_message}");
    false
}

/// Ask the user to confirm with [Y/n]. Returns `false` if the user typed "n".
fn confirm(prompt: &str) -> Result<bool> {
    print!("  {prompt} [Y/n] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_lowercase() != "n")
}

/// Show the updated MCP matrix after an operation.
fn show_updated_table(home: &Path, detected: &[&Platform]) {
    let post_configs = collect_all_platform_configs(home, detected);
    print_mcp_table(detected, &post_configs);
}

// ── Test-only re-exports ──────────────────────────────────────────────────

#[cfg(test)]
use add::build_server_config;
#[cfg(test)]
use parse::{parse_kv_map, parse_pasted_servers, split_command_line};
#[cfg(test)]
use platform_config::{
    existing_server_names, initial_platform_config, is_toml_platform, primary_servers_key,
    resolve_platform_config_path, target_platforms_for_server,
};
#[cfg(test)]
use remove::collect_all_server_names;
#[cfg(test)]
use render::{
    render_mcp_cards, render_mcp_matrix, render_mcp_table, server_presence_icon, DEFAULT_TERM_WIDTH,
};
#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use sync::{build_unified_servers, collect_missing_servers, print_sync_summary};
#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::Platform;

    #[test]
    fn registry_source_label_shows_local_path_when_set() {
        assert_eq!(
            registry_source_label(Some(Path::new("/tmp/r"))),
            "local: /tmp/r"
        );
    }

    #[test]
    fn registry_source_label_shows_github_when_unset() {
        assert_eq!(registry_source_label(None), "github");
    }

    fn test_platform(name: &str) -> Platform {
        Platform {
            name: name.to_string(),
            config_path: format!("{name}.json"),
            config_format: Some("json".to_string()),
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec!["mcpServers".to_string()],
            deprecated_keys: Vec::new(),
            unsupported_keys: Vec::new(),
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            instruction_precedence: vec![],
            cli: None,

            provider: None,
            tool_name: None,
        }
    }

    /// 8 platforms with reasonably long names and 10 MCP servers: the matrix
    /// layout would need well over 120 columns, so this must render as cards.
    #[test]
    fn render_mcp_table_switches_to_cards_for_many_platforms() {
        let platforms: Vec<Platform> = (1..=8)
            .map(|i| test_platform(&format!("platform-name-{i:02}")))
            .collect();
        let detected: Vec<&Platform> = platforms.iter().collect();

        let server_names: Vec<String> = (1..=10).map(|i| format!("mcp-server-{i:02}")).collect();

        let mut all_configs: PlatformConfigs = BTreeMap::new();
        for (p_idx, platform) in platforms.iter().enumerate() {
            let mut servers = BTreeMap::new();
            for (s_idx, server) in server_names.iter().enumerate() {
                // Deterministic, mixed presence pattern.
                if (p_idx + s_idx) % 2 == 0 {
                    servers.insert(server.clone(), serde_json::json!({}));
                }
            }
            all_configs.insert(platform.name.clone(), servers);
        }

        let rendered = render_mcp_table(&detected, &all_configs, DEFAULT_TERM_WIDTH);

        for line in rendered.lines() {
            let visible_len = strip_ansi(line).chars().count();
            assert!(
                visible_len <= 120,
                "line exceeds 120 visible chars ({visible_len}): {line:?}"
            );
        }

        for (p_idx, platform) in platforms.iter().enumerate() {
            assert!(
                rendered.contains(&platform.name),
                "missing platform section for {}",
                platform.name
            );
            for (s_idx, server) in server_names.iter().enumerate() {
                let expects_present = (p_idx + s_idx) % 2 == 0;
                let icon = if expects_present { '✓' } else { '✗' };
                let needle = server.to_string();
                let section_start = rendered.find(&platform.name).unwrap();
                let next_section = platforms
                    .get(p_idx + 1)
                    .and_then(|next| rendered.find(&next.name))
                    .unwrap_or(rendered.len());
                let section = &rendered[section_start..next_section];
                let line = section
                    .lines()
                    .find(|l| l.contains(&needle))
                    .unwrap_or_else(|| panic!("missing {server} in section for {}", platform.name));
                assert!(
                    line.contains(icon),
                    "expected {icon} for {server} in {}, got: {line:?}",
                    platform.name
                );
            }
        }
    }

    #[test]
    fn render_mcp_table_uses_matrix_for_few_platforms() {
        let platforms = [test_platform("cursor"), test_platform("claude")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_configs: PlatformConfigs = BTreeMap::new();
        let mut cursor_servers = BTreeMap::new();
        cursor_servers.insert("fs".to_string(), serde_json::json!({}));
        all_configs.insert("cursor".to_string(), cursor_servers);
        all_configs.insert("claude".to_string(), BTreeMap::new());

        let rendered = render_mcp_table(&detected, &all_configs, DEFAULT_TERM_WIDTH);
        assert!(rendered.contains("Server"));
        assert!(rendered.contains('✓'));
        assert!(rendered.contains('✗'));
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for esc in chars.by_ref() {
                    if esc == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    // ── build_unified_servers ─────────────────────────────────────────────

    #[test]
    fn build_unified_servers_empty_when_no_configs() {
        let configs: PlatformConfigs = BTreeMap::new();
        let unified = build_unified_servers(&configs);
        assert!(unified.is_empty());
    }

    #[test]
    fn build_unified_servers_collects_from_single_platform() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("github".to_string(), serde_json::json!({"url": "a"}));
        servers.insert("slack".to_string(), serde_json::json!({"url": "b"}));
        configs.insert("cursor".to_string(), servers);

        let unified = build_unified_servers(&configs);
        assert_eq!(unified.len(), 2);
        assert!(unified.contains_key("github"));
        assert!(unified.contains_key("slack"));
    }

    #[test]
    fn build_unified_servers_first_platform_wins_on_duplicate_names() {
        let mut configs: PlatformConfigs = BTreeMap::new();

        let mut cursor = BTreeMap::new();
        cursor.insert(
            "github".to_string(),
            serde_json::json!({"url": "from-cursor"}),
        );
        configs.insert("cursor".to_string(), cursor);

        let mut claude = BTreeMap::new();
        claude.insert(
            "github".to_string(),
            serde_json::json!({"url": "from-claude"}),
        );
        configs.insert("claude".to_string(), claude);

        let unified = build_unified_servers(&configs);
        let (val, platform) = unified.get("github").unwrap();
        // BTreeMap iterates in sorted key order, so "claude" comes before "cursor"
        assert_eq!(platform, &"claude");
        assert_eq!(val, &serde_json::json!({"url": "from-claude"}));
    }

    #[test]
    fn build_unified_servers_preserves_source_platform_ref() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("x".to_string(), serde_json::json!({}));
        configs.insert("myplatform".to_string(), servers);

        let unified = build_unified_servers(&configs);
        assert_eq!(unified.get("x").unwrap().1, "myplatform");
    }

    // ── collect_missing_servers ───────────────────────────────────────────

    #[test]
    fn collect_missing_servers_empty_when_all_in_sync() {
        let platforms = [test_platform("cursor")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("github".to_string(), serde_json::json!({}));
        all_configs.insert("cursor".to_string(), servers);

        let mut unified: UnifiedServers = BTreeMap::new();
        unified.insert("github".to_string(), (serde_json::json!({}), "cursor"));

        let missing = collect_missing_servers(&detected, &all_configs, &unified);
        assert!(missing.is_empty());
    }

    #[test]
    fn collect_missing_servers_finds_gaps() {
        let platforms = [test_platform("cursor"), test_platform("claude")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_configs: PlatformConfigs = BTreeMap::new();
        let mut cursor = BTreeMap::new();
        cursor.insert("github".to_string(), serde_json::json!({}));
        all_configs.insert("cursor".to_string(), cursor);
        all_configs.insert("claude".to_string(), BTreeMap::new());

        let mut unified: UnifiedServers = BTreeMap::new();
        unified.insert("github".to_string(), (serde_json::json!({}), "cursor"));

        let missing = collect_missing_servers(&detected, &all_configs, &unified);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "claude");
        assert_eq!(missing[0].1, "github");
    }

    #[test]
    fn collect_missing_servers_empty_when_no_platforms() {
        let detected: Vec<&Platform> = vec![];
        let all_configs: PlatformConfigs = BTreeMap::new();
        let unified: UnifiedServers = BTreeMap::new();

        let missing = collect_missing_servers(&detected, &all_configs, &unified);
        assert!(missing.is_empty());
    }

    #[test]
    fn collect_missing_servers_handles_platform_absent_from_configs() {
        let platforms = [test_platform("cursor"), test_platform("claude")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        // "claude" has no entry at all in all_configs
        let mut all_configs: PlatformConfigs = BTreeMap::new();
        let mut cursor = BTreeMap::new();
        cursor.insert("github".to_string(), serde_json::json!({}));
        all_configs.insert("cursor".to_string(), cursor);

        let mut unified: UnifiedServers = BTreeMap::new();
        unified.insert("github".to_string(), (serde_json::json!({}), "cursor"));

        let missing = collect_missing_servers(&detected, &all_configs, &unified);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "claude");
    }

    #[test]
    fn collect_missing_servers_multiple_servers_missing_on_multiple_platforms() {
        let platforms = [test_platform("a"), test_platform("b")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let all_configs: PlatformConfigs = BTreeMap::new();
        // Neither platform has any servers
        let mut unified: UnifiedServers = BTreeMap::new();
        unified.insert("s1".to_string(), (serde_json::json!({}), "a"));
        unified.insert("s2".to_string(), (serde_json::json!({}), "b"));

        let missing = collect_missing_servers(&detected, &all_configs, &unified);
        // 2 platforms x 2 servers = 4 missing entries
        assert_eq!(missing.len(), 4);
    }

    // ── existing_server_names ─────────────────────────────────────────────

    #[test]
    fn existing_server_names_returns_empty_for_unknown_platform() {
        let configs: PlatformConfigs = BTreeMap::new();
        let names = existing_server_names(&configs, "nonexistent");
        assert!(names.is_empty());
    }

    #[test]
    fn existing_server_names_returns_all_names() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("a".to_string(), serde_json::json!({}));
        servers.insert("b".to_string(), serde_json::json!({}));
        servers.insert("c".to_string(), serde_json::json!({}));
        configs.insert("cursor".to_string(), servers);

        let names = existing_server_names(&configs, "cursor");
        assert_eq!(names.len(), 3);
        assert!(names.contains("a"));
        assert!(names.contains("b"));
        assert!(names.contains("c"));
    }

    // ── collect_all_server_names ──────────────────────────────────────────

    #[test]
    fn collect_all_server_names_empty() {
        let configs: PlatformConfigs = BTreeMap::new();
        let names = collect_all_server_names(&configs);
        assert!(names.is_empty());
    }

    #[test]
    fn collect_all_server_names_deduplicates_across_platforms() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut cursor = BTreeMap::new();
        cursor.insert("github".to_string(), serde_json::json!({}));
        cursor.insert("slack".to_string(), serde_json::json!({}));
        configs.insert("cursor".to_string(), cursor);

        let mut claude = BTreeMap::new();
        claude.insert("github".to_string(), serde_json::json!({}));
        claude.insert("notion".to_string(), serde_json::json!({}));
        configs.insert("claude".to_string(), claude);

        let names = collect_all_server_names(&configs);
        assert_eq!(names.len(), 3);
        assert!(names.contains("github"));
        assert!(names.contains("slack"));
        assert!(names.contains("notion"));
    }

    #[test]
    fn collect_all_server_names_deterministic_order() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut s1 = BTreeMap::new();
        s1.insert("z-server".to_string(), serde_json::json!({}));
        s1.insert("a-server".to_string(), serde_json::json!({}));
        configs.insert("p1".to_string(), s1);

        let names: Vec<String> = collect_all_server_names(&configs).into_iter().collect();
        assert_eq!(names, vec!["a-server", "z-server"]);
    }

    // ── target_platforms_for_server ───────────────────────────────────────

    #[test]
    fn target_platforms_for_server_none_when_absent() {
        let platforms = [test_platform("a"), test_platform("b")];
        let detected: Vec<&Platform> = platforms.iter().collect();
        let all_configs: PlatformConfigs = BTreeMap::new();

        let targets = target_platforms_for_server(&detected, &all_configs, "x");
        assert!(targets.is_empty());
    }

    #[test]
    fn target_platforms_for_server_filters_correctly() {
        let platforms = [test_platform("a"), test_platform("b"), test_platform("c")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_configs: PlatformConfigs = BTreeMap::new();
        let mut a_servers = BTreeMap::new();
        a_servers.insert("github".to_string(), serde_json::json!({}));
        all_configs.insert("a".to_string(), a_servers);
        // "b" has no servers
        let mut c_servers = BTreeMap::new();
        c_servers.insert("github".to_string(), serde_json::json!({}));
        all_configs.insert("c".to_string(), c_servers);

        let targets = target_platforms_for_server(&detected, &all_configs, "github");
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().any(|p| p.name == "a"));
        assert!(targets.iter().any(|p| p.name == "c"));
        assert!(!targets.iter().any(|p| p.name == "b"));
    }

    // ── resolve_platform_config_path ──────────────────────────────────────

    #[test]
    fn resolve_platform_config_path_returns_primary_when_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_file = home.join("test.json");
        std::fs::write(&config_file, "{}").unwrap();

        let platform = Platform {
            config_path: "test.json".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved, config_file);
    }

    #[test]
    fn resolve_platform_config_path_falls_back_jsonc_to_json() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Primary is .jsonc, but .json exists
        std::fs::write(home.join("cfg.json"), "{}").unwrap();

        let platform = Platform {
            config_path: "cfg.jsonc".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved.extension().unwrap(), "json");
    }

    #[test]
    fn resolve_platform_config_path_falls_back_json_to_jsonc() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("cfg.jsonc"), "{}").unwrap();

        let platform = Platform {
            config_path: "cfg.json".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved.extension().unwrap(), "jsonc");
    }

    #[test]
    fn resolve_platform_config_path_returns_primary_when_neither_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        let platform = Platform {
            config_path: "nonexistent.json".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved, home.join("nonexistent.json"));
    }

    #[test]
    fn resolve_platform_config_path_primary_takes_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::write(home.join("cfg.jsonc"), "{}").unwrap();
        std::fs::write(home.join("cfg.json"), "{}").unwrap();

        let platform = Platform {
            config_path: "cfg.jsonc".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved.extension().unwrap(), "jsonc");
    }

    #[test]
    fn resolve_platform_config_path_unknown_extension_no_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // .yaml doesn't have a known alternate
        let platform = Platform {
            config_path: "cfg.yaml".to_string(),
            ..test_platform("test")
        };

        let resolved = resolve_platform_config_path(home, &platform);
        assert_eq!(resolved, home.join("cfg.yaml"));
    }

    // ── validate_required_input ───────────────────────────────────────────

    #[test]
    fn validate_required_input_passes_non_empty() {
        assert!(validate_required_input("hello", "err"));
    }

    #[test]
    fn validate_required_input_rejects_empty() {
        assert!(!validate_required_input("", "err"));
    }

    #[test]
    fn validate_required_input_accepts_whitespace_only() {
        // trim happens at the caller level, so raw whitespace is "valid" here
        assert!(validate_required_input("  ", "err"));
    }

    #[test]
    fn validate_required_input_accepts_single_char() {
        assert!(validate_required_input("a", "err"));
    }

    // ── build_server_config ───────────────────────────────────────────────

    #[test]
    fn build_server_config_url_type() {
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "http".to_string(),
            transport: ServerTransport::Url {
                url: "https://example.com/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        };

        let config = build_server_config(&input);
        assert_eq!(config["type"], "http");
        assert_eq!(config["url"], "https://example.com/mcp");
        assert!(config.get("command").is_none());
    }

    #[test]
    fn build_server_config_stdio_type_with_args() {
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "stdio".to_string(),
            transport: ServerTransport::Command {
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "@scope/server".to_string()],
                env: BTreeMap::new(),
            },
        };

        let config = build_server_config(&input);
        assert_eq!(config["type"], "stdio");
        assert_eq!(config["command"], "npx");
        assert_eq!(config["args"], serde_json::json!(["-y", "@scope/server"]));
        assert!(config.get("url").is_none());
    }

    #[test]
    fn build_server_config_stdio_no_args() {
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "stdio".to_string(),
            transport: ServerTransport::Command {
                command: "node".to_string(),
                args: vec![],
                env: BTreeMap::new(),
            },
        };

        let config = build_server_config(&input);
        assert_eq!(config["command"], "node");
        assert_eq!(config["args"], serde_json::json!([]));
    }

    // ── server_presence_icon ──────────────────────────────────────────────

    #[test]
    fn server_presence_icon_checkmark() {
        let icon = server_presence_icon(true);
        assert!(icon.contains('✓'));
    }

    #[test]
    fn server_presence_icon_cross() {
        let icon = server_presence_icon(false);
        assert!(icon.contains('✗'));
    }

    #[test]
    fn server_presence_icon_has_color_codes() {
        let icon = server_presence_icon(true);
        assert!(icon.contains("\x1b[32m")); // green
        let icon = server_presence_icon(false);
        assert!(icon.contains("\x1b[31m")); // red
    }

    // ── is_toml_platform ─────────────────────────────────────────────────

    #[test]
    fn is_toml_platform_true_when_format_is_toml() {
        let p = Platform {
            config_format: Some("toml".to_string()),
            ..test_platform("x")
        };
        assert!(is_toml_platform(&p));
    }

    #[test]
    fn is_toml_platform_false_for_json() {
        let p = Platform {
            config_format: Some("json".to_string()),
            ..test_platform("x")
        };
        assert!(!is_toml_platform(&p));
    }

    #[test]
    fn is_toml_platform_false_for_none() {
        let p = Platform {
            config_format: None,
            ..test_platform("x")
        };
        assert!(!is_toml_platform(&p));
    }

    // ── primary_servers_key ───────────────────────────────────────────────

    #[test]
    fn primary_servers_key_uses_first_key() {
        let p = Platform {
            mcp_servers_key: vec!["servers".to_string(), "mcpServers".to_string()],
            ..test_platform("x")
        };
        assert_eq!(primary_servers_key(&p), "servers");
    }

    #[test]
    fn primary_servers_key_defaults_to_mcp_servers_when_empty() {
        let p = Platform {
            mcp_servers_key: vec![],
            ..test_platform("x")
        };
        assert_eq!(primary_servers_key(&p), "mcpServers");
    }

    // ── initial_platform_config ───────────────────────────────────────────

    #[test]
    fn initial_platform_config_toml_returns_empty_string() {
        let p = Platform {
            config_format: Some("toml".to_string()),
            ..test_platform("x")
        };
        assert_eq!(initial_platform_config(&p), "");
    }

    #[test]
    fn initial_platform_config_json_uses_primary_key() {
        let p = Platform {
            config_format: Some("json".to_string()),
            mcp_servers_key: vec!["mcpServers".to_string()],
            ..test_platform("x")
        };
        let cfg = initial_platform_config(&p);
        assert!(cfg.contains("\"mcpServers\""));
        assert!(cfg.ends_with('\n'));
    }

    #[test]
    fn initial_platform_config_json_custom_key() {
        let p = Platform {
            config_format: Some("json".to_string()),
            mcp_servers_key: vec!["servers".to_string()],
            ..test_platform("x")
        };
        let cfg = initial_platform_config(&p);
        assert!(cfg.contains("\"servers\""));
    }

    // ── render_mcp_table ──────────────────────────────────────────────────

    #[test]
    fn render_mcp_table_empty_message() {
        let detected: Vec<&Platform> = vec![];
        let configs: PlatformConfigs = BTreeMap::new();
        let rendered = render_mcp_table(&detected, &configs, DEFAULT_TERM_WIDTH);
        assert!(rendered.contains("No MCP servers configured."));
    }

    #[test]
    fn render_mcp_table_contains_server_names() {
        let platforms = [test_platform("cursor")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("github".to_string(), serde_json::json!({}));
        servers.insert("slack".to_string(), serde_json::json!({}));
        configs.insert("cursor".to_string(), servers);

        let rendered = render_mcp_table(&detected, &configs, DEFAULT_TERM_WIDTH);
        assert!(rendered.contains("github"));
        assert!(rendered.contains("slack"));
    }

    #[test]
    fn render_mcp_table_shows_checkmarks_for_present() {
        let platforms = [test_platform("cursor")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut servers = BTreeMap::new();
        servers.insert("github".to_string(), serde_json::json!({}));
        configs.insert("cursor".to_string(), servers);

        let rendered = render_mcp_table(&detected, &configs, DEFAULT_TERM_WIDTH);
        assert!(rendered.contains('✓'));
    }

    // ── render_mcp_cards ──────────────────────────────────────────────────

    #[test]
    fn render_mcp_cards_shows_platform_headers() {
        let platforms = [test_platform("p1"), test_platform("p2")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_servers = BTreeSet::new();
        all_servers.insert("github".to_string());

        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut s1 = BTreeMap::new();
        s1.insert("github".to_string(), serde_json::json!({}));
        configs.insert("p1".to_string(), s1);
        configs.insert("p2".to_string(), BTreeMap::new());

        let rendered = render_mcp_cards(&all_servers, &detected, &configs, 6);
        assert!(rendered.contains("p1"));
        assert!(rendered.contains("p2"));
        // p1 has github -> checkmark, p2 doesn't -> cross
        let p1_section_start = rendered.find("p1").unwrap();
        let p2_section_start = rendered.find("p2").unwrap();
        let p1_section = &rendered[p1_section_start..p2_section_start];
        assert!(p1_section.contains('✓'));
        let p2_section = &rendered[p2_section_start..];
        assert!(p2_section.contains('✗'));
    }

    // ── render_mcp_matrix ─────────────────────────────────────────────────

    #[test]
    fn render_mcp_matrix_header_row() {
        let platforms = [test_platform("cursor"), test_platform("claude")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_servers = BTreeSet::new();
        all_servers.insert("github".to_string());

        let mut configs: PlatformConfigs = BTreeMap::new();
        let mut cursor = BTreeMap::new();
        cursor.insert("github".to_string(), serde_json::json!({}));
        configs.insert("cursor".to_string(), cursor);
        configs.insert("claude".to_string(), BTreeMap::new());

        let rendered = render_mcp_matrix(&all_servers, &detected, &configs, 8, 6);
        // Header should contain "Server" and both platform names
        let first_line = rendered.lines().next().unwrap();
        assert!(first_line.contains("Server"));
        assert!(strip_ansi(first_line).contains("cursor"));
        assert!(strip_ansi(first_line).contains("claude"));
    }

    #[test]
    fn render_mcp_matrix_separator_line() {
        let platforms = [test_platform("a")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let mut all_servers = BTreeSet::new();
        all_servers.insert("x".to_string());

        let configs: PlatformConfigs = BTreeMap::new();
        let rendered = render_mcp_matrix(&all_servers, &detected, &configs, 4, 4);

        // Second line should be a separator (all ─)
        let sep_line = rendered.lines().nth(1).unwrap();
        assert!(sep_line.contains('─'));
    }

    // ── strip_ansi (test helper) ──────────────────────────────────────────

    #[test]
    fn strip_ansi_removes_escape_codes() {
        let colored = "\x1b[32m ✓\x1b[0m";
        assert_eq!(strip_ansi(colored), " ✓");
    }

    #[test]
    fn strip_ansi_plain_text_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn strip_ansi_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_multiple_codes() {
        let s = "\x1b[1m\x1b[32mtext\x1b[0m";
        assert_eq!(strip_ansi(s), "text");
    }

    // ── test_platform helper edge cases ───────────────────────────────────

    #[test]
    fn test_platform_default_values() {
        let p = test_platform("my-platform");
        assert_eq!(p.name, "my-platform");
        assert_eq!(p.config_path, "my-platform.json");
        assert_eq!(p.config_format, Some("json".to_string()));
        assert!(!p.toml_array_format);
        assert_eq!(p.command_format, "separate");
        assert_eq!(p.mcp_servers_key, vec!["mcpServers".to_string()]);
        assert!(p.deprecated_keys.is_empty());
        assert!(p.unsupported_keys.is_empty());
        assert!(p.fields_mapping.is_empty());
        assert!(p.required_fields.is_empty());
        assert!(p.server_extras.is_empty());
        assert!(p.skills_dir.is_none());
        assert!(p.instruction_file.is_none());
        assert!(p.cli.is_none());
    }

    // ── WizardAction enum ─────────────────────────────────────────────────

    #[test]
    fn wizard_action_clone_copy() {
        let a = WizardAction::Sync;
        let b = a;
        // Copy semantics — both should be valid
        match (a, b) {
            (WizardAction::Sync, WizardAction::Sync) => {}
            _ => panic!("Copy failed"),
        }
    }

    // ── ServerTransport enum ──────────────────────────────────────────────

    #[test]
    fn server_transport_url_variants() {
        let t = ServerTransport::Url {
            url: "https://x.com".to_string(),
            headers: BTreeMap::new(),
        };
        match t {
            ServerTransport::Url { url, .. } => assert_eq!(url, "https://x.com"),
            _ => panic!("Expected Url variant"),
        }
    }

    #[test]
    fn server_transport_command_variants() {
        let t = ServerTransport::Command {
            command: "npx".to_string(),
            args: vec!["-y".to_string()],
            env: BTreeMap::new(),
        };
        match t {
            ServerTransport::Command { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["-y"]);
            }
            _ => panic!("Expected Command variant"),
        }
    }

    // ── split_command_line ────────────────────────────────────────────────

    #[test]
    fn command_line_splitting_logic() {
        let (command, args) = split_command_line("npx -y @scope/server --verbose").unwrap();

        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y", "@scope/server", "--verbose"]);
    }

    #[test]
    fn command_line_single_word() {
        let (command, args) = split_command_line("node").unwrap();

        assert_eq!(command, "node");
        assert!(args.is_empty());
    }

    #[test]
    fn command_line_empty_string() {
        assert!(split_command_line("").is_err());
    }

    #[test]
    fn command_line_only_whitespace() {
        assert!(split_command_line("   ").is_err());
    }

    #[test]
    fn command_line_extra_whitespace() {
        let (command, args) = split_command_line("  npx   -y   @scope/server  ").unwrap();

        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y", "@scope/server"]);
    }

    // ── cancelled_prompt ──────────────────────────────────────────────────

    #[test]
    fn cancelled_prompt_contains_message() {
        let err = cancelled_prompt("user pressed Esc");
        assert!(err.to_string().contains("user pressed Esc"));
        assert!(err.to_string().contains("Cancelled"));
    }

    // ── AddServerInput struct ─────────────────────────────────────────────

    #[test]
    fn add_server_input_fields() {
        let input = AddServerInput {
            name: "github".to_string(),
            server_type: "http".to_string(),
            transport: ServerTransport::Url {
                url: "https://x.com".to_string(),
                headers: BTreeMap::new(),
            },
        };
        assert_eq!(input.name, "github");
        assert_eq!(input.server_type, "http");
    }

    // ── PlatformConfigs / UnifiedServers type aliases ─────────────────────

    #[test]
    fn platform_configs_btreemap_ordering() {
        let mut configs: PlatformConfigs = BTreeMap::new();
        configs.insert("z-platform".to_string(), BTreeMap::new());
        configs.insert("a-platform".to_string(), BTreeMap::new());

        let keys: Vec<&String> = configs.keys().collect();
        assert_eq!(keys, vec!["a-platform", "z-platform"]);
    }

    #[test]
    fn unified_servers_btreemap_ordering() {
        let mut unified: UnifiedServers = BTreeMap::new();
        unified.insert("z-server".to_string(), (serde_json::json!({}), "p"));
        unified.insert("a-server".to_string(), (serde_json::json!({}), "p"));

        let keys: Vec<&String> = unified.keys().collect();
        assert_eq!(keys, vec!["a-server", "z-server"]);
    }

    // ── collect_all_platform_configs ──────────────────────────────────────

    #[test]
    fn collect_all_platform_configs_returns_map_per_platform() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        let platforms = [test_platform("cursor"), test_platform("claude")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let configs = collect_all_platform_configs(home, &detected);
        assert_eq!(configs.len(), 2);
        assert!(configs.contains_key("cursor"));
        assert!(configs.contains_key("claude"));
    }

    #[test]
    fn collect_all_platform_configs_empty_when_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        let platforms = [test_platform("cursor")];
        let detected: Vec<&Platform> = platforms.iter().collect();

        let configs = collect_all_platform_configs(home, &detected);
        // Config file doesn't exist, so it should return empty map
        assert!(configs["cursor"].is_empty());
    }

    // ── print_sync_summary edge cases ─────────────────────────────────────

    #[test]
    fn print_sync_summary_no_output_panic_test() {
        // Just verify the function doesn't panic with zero errors
        print_sync_summary(5, 0);
    }

    #[test]
    fn print_sync_summary_with_errors() {
        // Verify it doesn't panic with errors
        print_sync_summary(3, 2);
    }

    // ── type alias constraints ────────────────────────────────────────────

    #[test]
    fn missing_servers_is_vector_of_tuples() {
        let missing: MissingServers = vec![("platform".to_string(), "server".to_string())];
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, "platform");
        assert_eq!(missing[0].1, "server");
    }

    // ── Edge: multiple mcp_servers_key paths ──────────────────────────────

    #[test]
    fn primary_servers_key_uses_first_of_multiple() {
        let p = Platform {
            mcp_servers_key: vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
            ..test_platform("x")
        };
        assert_eq!(primary_servers_key(&p), "first");
    }

    // ── Edge: server_presence_icon exact output ───────────────────────────

    #[test]
    fn server_presence_icon_true_exact() {
        assert_eq!(server_presence_icon(true), "\x1b[32m ✓\x1b[0m");
    }

    #[test]
    fn server_presence_icon_false_exact() {
        assert_eq!(server_presence_icon(false), "\x1b[31m ✗\x1b[0m");
    }

    // ── parse_pasted_servers ──────────────────────────────────────────────

    #[test]
    fn parse_pasted_colab_fragment() {
        // The exact snippet from the colab-mcp README, without outer braces,
        // as pasted line-by-line (read_pasted_text joins lines with '\n').
        let text = r#""mcpServers": {
  "colab-mcp": {
    "command": "uvx",
    "args": ["git+https://github.com/googlecolab/colab-mcp"],
    "timeout": 30000
  }
}"#;
        let servers = parse_pasted_servers(text).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "colab-mcp");
        assert_eq!(servers[0].1["command"], "uvx");
        assert_eq!(
            servers[0].1["args"],
            serde_json::json!(["git+https://github.com/googlecolab/colab-mcp"])
        );
        assert_eq!(servers[0].1["timeout"], 30000);
    }

    #[test]
    fn parse_pasted_colab_wrapped() {
        let text = r#"{ "mcpServers": { "colab-mcp": { "command": "uvx", "args": ["git+https://github.com/googlecolab/colab-mcp"], "timeout": 30000 } } }"#;
        let servers = parse_pasted_servers(text).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "colab-mcp");
        assert_eq!(servers[0].1["command"], "uvx");
        assert_eq!(
            servers[0].1["args"],
            serde_json::json!(["git+https://github.com/googlecolab/colab-mcp"])
        );
        assert_eq!(servers[0].1["timeout"], 30000);
    }

    #[test]
    fn parse_pasted_fragment_with_trailing_comma() {
        // Requirement 2c: trailing comma removed before wrapping.
        let text = r#""mcpServers": { "colab-mcp": { "command": "uvx" } },"#;
        let servers = parse_pasted_servers(text).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "colab-mcp");
        assert_eq!(servers[0].1["command"], "uvx");
    }

    #[test]
    fn parse_pasted_bare_name_map_two_servers() {
        let text =
            r#"{"a": {"url": "https://x/mcp"}, "b": {"command": "npx", "args": ["-y","b"]}}"#;
        let servers = parse_pasted_servers(text).unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0].0, "a");
        assert_eq!(servers[1].0, "b");
        assert_eq!(servers[0].1["url"], "https://x/mcp");
        assert_eq!(servers[1].1["command"], "npx");
    }

    #[test]
    fn parse_pasted_invalid_entry_names_server() {
        let err = parse_pasted_servers(r#"{"mcpServers": {"bad": {"foo": 1}}}"#).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("bad"), "error was: {err}");
        assert!(
            message.contains("command") && message.contains("url"),
            "error was: {err}"
        );
    }

    #[test]
    fn parse_pasted_empty_is_no_servers_found() {
        let err = parse_pasted_servers("{}").unwrap_err();
        assert!(
            err.to_string().contains("no servers found"),
            "error was: {err}"
        );
    }

    #[test]
    fn split_command_line_keeps_quotes() {
        let (command, args) = split_command_line(r#"uvx "my server" --flag"#).unwrap();
        assert_eq!(command, "uvx");
        assert_eq!(args, vec!["my server", "--flag"]);
    }

    #[test]
    fn parse_kv_map_two_entries() {
        let map = parse_kv_map(r#"A=1 B="two words""#).unwrap();
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("B").map(String::as_str), Some("two words"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_kv_map_rejects_missing_equals() {
        assert!(parse_kv_map("NOEQUALS").is_err());
    }

    #[test]
    fn parse_kv_map_rejects_empty_key() {
        assert!(parse_kv_map("=v").is_err());
    }

    #[test]
    fn build_server_config_stdio_omits_empty_env() {
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "stdio".to_string(),
            transport: ServerTransport::Command {
                command: "npx".to_string(),
                args: vec!["-y".to_string()],
                env: BTreeMap::new(),
            },
        };
        let config = build_server_config(&input);
        assert!(config.get("env").is_none());
    }

    #[test]
    fn build_server_config_stdio_emits_env() {
        let mut env = BTreeMap::new();
        env.insert("K".to_string(), "V".to_string());
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "stdio".to_string(),
            transport: ServerTransport::Command {
                command: "npx".to_string(),
                args: vec![],
                env,
            },
        };
        let config = build_server_config(&input);
        assert_eq!(config["env"], serde_json::json!({"K": "V"}));
    }

    #[test]
    fn build_server_config_http_omits_empty_headers() {
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "http".to_string(),
            transport: ServerTransport::Url {
                url: "https://example.com/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        };
        let config = build_server_config(&input);
        assert_eq!(config["type"], "http");
        assert!(config.get("headers").is_none());
    }

    #[test]
    fn build_server_config_http_emits_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), "Bearer x".to_string());
        let input = AddServerInput {
            name: "test".to_string(),
            server_type: "http".to_string(),
            transport: ServerTransport::Url {
                url: "https://example.com/mcp".to_string(),
                headers,
            },
        };
        let config = build_server_config(&input);
        assert_eq!(
            config["headers"],
            serde_json::json!({"Authorization": "Bearer x"})
        );
    }

    #[test]
    fn build_server_config_pasted_is_verbatim() {
        let entry = serde_json::json!({"command": "uvx", "timeout": 30000});
        let input = AddServerInput {
            name: "colab-mcp".to_string(),
            server_type: "stdio".to_string(),
            transport: ServerTransport::Pasted(entry.clone()),
        };
        let config = build_server_config(&input);
        assert_eq!(config, entry);
    }
}
