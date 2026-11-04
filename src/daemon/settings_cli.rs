//! CLI handlers for `canopy config` subcommands.
//!
//! Every subcommand delegates to the MCP tool of the same purpose via
//! `daemon::cli_daemon::call_tool`, so the CLI and MCP share one code path
//! — including validation and the single writer.

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;

#[derive(Subcommand, Debug)]
pub(crate) enum SettingsAction {
    /// List every setting with its group, default, effective value and status.
    List {
        /// Only list settings in this group (e.g. "TUI & appearance").
        #[arg(long)]
        group: Option<String>,
    },
    /// Show one setting by its dotted key.
    Get {
        /// The setting's dotted key (e.g. `clean.retention_days`).
        key: String,
    },
    /// Change one setting (validated before anything is written).
    Set {
        /// The setting's dotted key.
        key: String,
        /// The new value.
        value: String,
    },
    /// Reset one setting to its default.
    Reset {
        /// The setting's dotted key.
        key: String,
    },
    /// Print the path of config.toml.
    Path,
}

pub async fn handle_settings_action(
    action: SettingsAction,
    port_override: Option<u16>,
) -> Result<()> {
    match action {
        SettingsAction::List { group } => {
            let mut args = serde_json::json!({ "action": "list" });
            if let Some(group) = group {
                args["group"] = serde_json::json!(group);
            }
            print_tool(port_override, "config", &args)
        }
        SettingsAction::Get { key } => print_tool(
            port_override,
            "config",
            &serde_json::json!({ "action": "get", "key": key }),
        ),
        SettingsAction::Set { key, value } => print_tool(
            port_override,
            "config",
            &serde_json::json!({ "action": "set", "key": key, "value": value }),
        ),
        SettingsAction::Reset { key } => print_tool(
            port_override,
            "config",
            &serde_json::json!({ "action": "reset", "key": key }),
        ),
        SettingsAction::Path => {
            println!(
                "{}",
                crate::ensure_data_dir()?.join("config.toml").display()
            );
            Ok(())
        }
    }
}

fn print_tool(port_override: Option<u16>, tool: &str, args: &serde_json::Value) -> Result<()> {
    let text = call_tool(port_override, tool, args)?;
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: SettingsAction,
    }

    #[test]
    fn list_parses_with_and_without_a_group() {
        let cli = TestCli::try_parse_from(["test", "list", "--group", "TUI & appearance"])
            .expect("should parse");
        match cli.action {
            SettingsAction::List { group } => {
                assert_eq!(group.as_deref(), Some("TUI & appearance"));
            }
            _ => panic!("expected List"),
        }
        let bare = TestCli::try_parse_from(["test", "list"]).expect("should parse");
        assert!(matches!(bare.action, SettingsAction::List { group: None }));
    }

    #[test]
    fn set_reset_get_and_path_parse() {
        let cli = TestCli::try_parse_from(["test", "set", "theme", "modern"]).expect("parse set");
        match cli.action {
            SettingsAction::Set { key, value } => {
                assert_eq!(key, "theme");
                assert_eq!(value, "modern");
            }
            _ => panic!("expected Set"),
        }

        let cli = TestCli::try_parse_from(["test", "reset", "theme"]).expect("parse reset");
        assert!(matches!(cli.action, SettingsAction::Reset { ref key } if key == "theme"));

        let cli = TestCli::try_parse_from(["test", "get", "theme"]).expect("parse get");
        assert!(matches!(cli.action, SettingsAction::Get { ref key } if key == "theme"));

        let cli = TestCli::try_parse_from(["test", "path"]).expect("parse path");
        assert!(matches!(cli.action, SettingsAction::Path));
    }

    #[test]
    fn set_requires_a_value() {
        assert!(TestCli::try_parse_from(["test", "set", "theme"]).is_err());
    }
}
