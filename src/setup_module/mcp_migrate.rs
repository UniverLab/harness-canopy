//! Rewrite legacy unauthenticated `canopy` MCP entries to the stdio bridge.
//!
//! After the daemon token became mandatory, every harness config that still
//! holds an older unauthenticated canopy entry (a `url` form, with or without
//! headers) is rewritten to the `canopy bridge` stdio form so the harness
//! keeps working without the token ever being written to a file.
//!
//! `canopy setup` and the MCP wizard (`canopy mcp`) run this migration over
//! the platform list they already hold. The function never reads the daemon
//! token and adds no config key.

use std::path::{Path, PathBuf};

use crate::config::McpConfigRegistry;
use crate::setup_module::models::{resolve_config_path, Platform};
use crate::setup_module::platform_adapter::{adapt_config, apply_upsert_to_platform};

/// Outcome of migrating one platform's config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The `canopy` entry is already the stdio bridge form.
    Current,
    /// A legacy entry was rewritten to the bridge form.
    Legacy,
    /// No config file or no `canopy` entry: nothing to do.
    NoEntry,
    /// The file exists but cannot be parsed: left byte-identical.
    Unparsable,
}

/// Rewrite legacy `canopy` entries to the stdio bridge for every platform.
///
/// Returns one outcome per platform, index-aligned with `platforms`, so
/// callers can print per-platform notices. Idempotent: a second run over
/// already-migrated files changes no byte.
pub fn migrate_canopy_entries(home: &Path, platforms: &[&Platform]) -> Vec<MigrationOutcome> {
    platforms
        .iter()
        .map(|platform| migrate_one_platform(home, platform))
        .collect()
}

fn migrate_one_platform(home: &Path, platform: &Platform) -> MigrationOutcome {
    let path = resolve_config_path(home, &platform.config_path);
    if !path.exists() {
        return MigrationOutcome::NoEntry;
    }
    let entry = match read_canopy_entry(&path, platform) {
        Some(Some(entry)) => entry,
        Some(None) => return MigrationOutcome::NoEntry,
        None => {
            return if file_parses(&path) {
                MigrationOutcome::NoEntry
            } else {
                MigrationOutcome::Unparsable
            };
        }
    };
    if is_current_bridge(&entry) {
        return MigrationOutcome::Current;
    }
    backup_once(&path);
    let adapted = adapt_config(&entry, platform, "canopy");
    let _ = apply_upsert_to_platform(platform, &path, "canopy", &adapted);
    MigrationOutcome::Legacy
}

/// Read the `canopy` entry: `Some(Some)` present, `Some(None)` no entry,
/// `None` when the file cannot be read through the platform's servers key.
fn read_canopy_entry(path: &Path, platform: &Platform) -> Option<Option<serde_json::Value>> {
    let config =
        McpConfigRegistry::extract_from_platform(&platform.name, path, &platform.mcp_servers_key)
            .ok()?;
    Some(
        config
            .servers
            .into_iter()
            .find(|server| server.name == "canopy")
            .map(|server| server.config),
    )
}

/// Check whether a config file parses (TOML by extension, JSON otherwise),
/// mirroring `McpConfigRegistry::extract_from_platform` without the key walk.
fn file_parses(path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
        return toml::from_str::<toml::Value>(&content).is_ok();
    }
    let clean = crate::setup_module::strip_jsonc_comments(&content);
    serde_json::from_str::<serde_json::Value>(&clean).is_ok()
}

/// Legacy transport keys whose presence means "unauthenticated url form".
const LEGACY_TRANSPORT_KEYS: &[&str] = &["url", "headers", "http_headers", "env_http_headers"];

/// A bridge entry carries none of the legacy url-form keys and runs
/// `canopy` with `bridge`, either split (`command` + `args`) or merged.
fn is_current_bridge(entry: &serde_json::Value) -> bool {
    let Some(obj) = entry.as_object() else {
        return false;
    };
    if LEGACY_TRANSPORT_KEYS
        .iter()
        .any(|key| obj.contains_key(*key))
    {
        return false;
    }
    has_bridge_command(obj)
}

fn has_bridge_command(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    match obj.get("command") {
        Some(serde_json::Value::String(command)) => {
            command == "canopy" && args_contain_bridge(obj.get("args"))
        }
        Some(serde_json::Value::Array(parts)) => {
            parts.first().and_then(|part| part.as_str()) == Some("canopy")
                && parts.iter().any(|part| part.as_str() == Some("bridge"))
        }
        _ => false,
    }
}

fn args_contain_bridge(args: Option<&serde_json::Value>) -> bool {
    args.and_then(|value| value.as_array())
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("bridge")))
}

/// Copy the file to `<file>.canopy-bak`, unless that backup already exists.
fn backup_once(path: &Path) {
    let backup = backup_path(path);
    if backup.exists() {
        return;
    }
    let _ = std::fs::copy(path, &backup);
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".canopy-bak");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_platform() -> Platform {
        Platform {
            name: "test".to_string(),
            config_path: "config.json".to_string(),
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
            user_instruction_file: None,
            cli: None,
            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
        }
    }

    fn merged_platform() -> Platform {
        let mut platform = test_platform();
        platform.name = "merged".to_string();
        platform.config_path = "merged.json".to_string();
        platform.command_format = "merged".to_string();
        platform
    }

    fn toml_platform() -> Platform {
        let mut platform = test_platform();
        platform.name = "codex-like".to_string();
        platform.config_path = ".codex/config.toml".to_string();
        platform.config_format = Some("toml".to_string());
        platform.mcp_servers_key = vec!["mcp_servers".to_string()];
        platform
    }

    fn write_config(home: &Path, rel: &str, content: &str) -> PathBuf {
        let path = home.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let content = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn rewrites_url_only_legacy_entry_to_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(
            dir.path(),
            "config.json",
            r#"{"mcpServers": {"canopy": {"url": "http://localhost:7755/mcp"}}}"#,
        );

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Legacy]);
        assert_eq!(
            read_json(&path)["mcpServers"]["canopy"],
            serde_json::json!({"command": "canopy", "args": ["bridge"]})
        );
        assert!(backup_path(&path).exists());
    }

    #[test]
    fn rewrites_url_with_headers_dropping_them_and_preserving_rest() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(
            dir.path(),
            "config.json",
            r#"{
                "version": 3,
                "mcpServers": {
                    "canopy": {
                        "url": "http://localhost:7755/mcp",
                        "headers": {"a": "b"},
                        "http_headers": {"c": "d"},
                        "env_http_headers": {"e": "f"},
                        "env": {"X": "1"}
                    },
                    "other": {"command": "uvx", "args": ["x"]}
                }
            }"#,
        );

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Legacy]);
        let root = read_json(&path);
        let canopy = &root["mcpServers"]["canopy"];
        assert_eq!(canopy["command"], serde_json::json!("canopy"));
        assert_eq!(canopy["args"], serde_json::json!(["bridge"]));
        for key in ["url", "headers", "http_headers", "env_http_headers"] {
            assert!(canopy.get(key).is_none(), "legacy key survived: {key}");
        }
        assert_eq!(canopy["env"], serde_json::json!({"X": "1"}));
        assert_eq!(
            root["mcpServers"]["other"],
            serde_json::json!({"command": "uvx", "args": ["x"]})
        );
        assert_eq!(root["version"], serde_json::json!(3));
    }

    #[test]
    fn leaves_bridge_entry_untouched_with_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(
            dir.path(),
            "config.json",
            r#"{"mcpServers": {"canopy": {"command": "canopy", "args": ["bridge"]}}}"#,
        );
        let before = std::fs::read(&path).unwrap();

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Current]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn merged_command_platform_rewritten_then_current() {
        let dir = tempfile::tempdir().unwrap();
        let platform = merged_platform();
        let path = write_config(
            dir.path(),
            "merged.json",
            r#"{"mcpServers": {"canopy": {"url": "http://localhost:7755/mcp"}}}"#,
        );

        let first = migrate_canopy_entries(dir.path(), &[&platform]);
        assert_eq!(first, vec![MigrationOutcome::Legacy]);
        let canopy = read_json(&path)["mcpServers"]["canopy"].clone();
        assert_eq!(canopy["command"], serde_json::json!(["canopy", "bridge"]));
        assert!(canopy.get("args").is_none());

        let second = migrate_canopy_entries(dir.path(), &[&platform]);
        assert_eq!(second, vec![MigrationOutcome::Current]);
    }

    #[test]
    fn toml_url_entry_rewritten_with_other_tables_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let platform = toml_platform();
        let path = write_config(
            dir.path(),
            ".codex/config.toml",
            "[mcp_servers.canopy]\nurl = \"http://localhost:7755/mcp\"\n\n[mcp_servers.other]\ncommand = \"uvx\"\n",
        );

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Legacy]);
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["canopy"]["command"].as_str(),
            Some("canopy")
        );
        assert!(parsed["mcp_servers"]["canopy"].get("url").is_none());
        assert_eq!(
            parsed["mcp_servers"]["other"]["command"].as_str(),
            Some("uvx")
        );
    }

    #[test]
    fn toml_bridge_entry_is_current_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let platform = toml_platform();
        let path = write_config(
            dir.path(),
            ".codex/config.toml",
            "[mcp_servers.canopy]\ncommand = \"canopy\"\nargs = [\"bridge\"]\n",
        );
        let before = std::fs::read(&path).unwrap();

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Current]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn backup_created_once_and_second_run_changes_no_byte() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(
            dir.path(),
            "config.json",
            r#"{"mcpServers": {"canopy": {"url": "http://localhost:7755/mcp"}}}"#,
        );

        let first = migrate_canopy_entries(dir.path(), &[&platform]);
        assert_eq!(first, vec![MigrationOutcome::Legacy]);
        let backup_bytes = std::fs::read(backup_path(&path)).unwrap();
        let migrated_bytes = std::fs::read(&path).unwrap();

        let second = migrate_canopy_entries(dir.path(), &[&platform]);
        assert_eq!(second, vec![MigrationOutcome::Current]);
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), backup_bytes);
        assert_eq!(std::fs::read(&path).unwrap(), migrated_bytes);
    }

    #[test]
    fn pre_existing_backup_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(
            dir.path(),
            "config.json",
            r#"{"mcpServers": {"canopy": {"url": "http://localhost:7755/mcp"}}}"#,
        );
        let backup = backup_path(&path);
        std::fs::write(&backup, b"sentinel").unwrap();

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Legacy]);
        assert_eq!(std::fs::read(&backup).unwrap(), b"sentinel");
        assert_eq!(
            read_json(&path)["mcpServers"]["canopy"],
            serde_json::json!({"command": "canopy", "args": ["bridge"]})
        );
    }

    #[test]
    fn unparsable_file_left_identical_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();
        let path = write_config(dir.path(), "config.json", "{\"mcpServers\": {\"canopy\": ");
        let before = std::fs::read(&path).unwrap();

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::Unparsable]);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn missing_file_is_skipped_without_creating_anything() {
        let dir = tempfile::tempdir().unwrap();
        let platform = test_platform();

        let outcomes = migrate_canopy_entries(dir.path(), &[&platform]);

        assert_eq!(outcomes, vec![MigrationOutcome::NoEntry]);
        assert!(!dir.path().join("config.json").exists());
    }

    #[test]
    fn outcomes_stay_index_aligned_across_platforms() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = test_platform();
        let mut plain = test_platform();
        plain.name = "plain".to_string();
        plain.config_path = "plain.json".to_string();
        write_config(
            dir.path(),
            "config.json",
            r#"{"mcpServers": {"canopy": {"url": "http://localhost:7755/mcp"}}}"#,
        );
        let plain_path = write_config(
            dir.path(),
            "plain.json",
            r#"{"mcpServers": {"other": {"command": "uvx"}}}"#,
        );
        let plain_before = std::fs::read(&plain_path).unwrap();

        let outcomes = migrate_canopy_entries(dir.path(), &[&legacy, &plain]);

        assert_eq!(
            outcomes,
            vec![MigrationOutcome::Legacy, MigrationOutcome::NoEntry]
        );
        assert_eq!(std::fs::read(&plain_path).unwrap(), plain_before);
    }
}
