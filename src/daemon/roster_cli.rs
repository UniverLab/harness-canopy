//! CLI handlers for `canopy roster` subcommands.
//!
//! Every subcommand delegates to the MCP tool of the same purpose via
//! `daemon::cli_daemon::call_tool`, so the CLI and MCP share one code path.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;

#[derive(Subcommand, Debug)]
pub(crate) enum RosterAction {
    /// List roster entries, optionally filtered to one platform.
    List {
        /// Restrict the listing to one platform.
        #[arg(long)]
        platform: Option<String>,
    },
    /// Declare a platform/model pair available or unavailable.
    Set {
        /// Platform to declare (e.g. `opencode-go`).
        platform: String,
        /// Model id or a prefix ending in `*`; omit for every model of the
        /// platform.
        model_pattern: Option<String>,
        /// Mark the pair available.
        #[arg(long, group = "state")]
        available: bool,
        /// Mark the pair unavailable.
        #[arg(long, group = "state")]
        unavailable: bool,
        /// Why the pair is unavailable.
        #[arg(long)]
        reason: Option<String>,
        /// RFC 3339 UTC instant the unavailability expires (with
        /// `--unavailable` only).
        #[arg(long)]
        until: Option<String>,
    },
    /// Delete one roster entry (idempotent).
    Clear {
        /// Platform whose entry to delete.
        platform: String,
        /// The exact pattern deleted; omit for the platform-wide entry.
        model_pattern: Option<String>,
    },
    /// Check the effective availability of one pair.
    Check {
        /// Platform to check.
        platform: String,
        /// Model to check; omit to check the platform-wide entry.
        model: Option<String>,
    },
}

pub async fn handle_roster_action(action: RosterAction, port_override: Option<u16>) -> Result<()> {
    match action {
        RosterAction::List { platform } => {
            let mut args = serde_json::json!({ "action": "roster_list" });
            if let Some(platform) = platform {
                args["platform"] = serde_json::json!(platform);
            }
            print_tool(port_override, "harness", &args)
        }
        RosterAction::Set {
            platform,
            model_pattern,
            available,
            unavailable,
            reason,
            until,
        } => {
            let state = resolve_state(available, unavailable)?;
            let mut args =
                serde_json::json!({ "action": "roster_set", "platform": platform, "state": state });
            if let Some(pattern) = model_pattern {
                args["model_pattern"] = serde_json::json!(pattern);
            }
            if let Some(reason) = reason {
                args["reason"] = serde_json::json!(reason);
            }
            if let Some(until) = until {
                args["until"] = serde_json::json!(until);
            }
            print_tool(port_override, "harness", &args)
        }
        RosterAction::Clear {
            platform,
            model_pattern,
        } => {
            let mut args = serde_json::json!({ "action": "roster_clear", "platform": platform });
            if let Some(pattern) = model_pattern {
                args["model_pattern"] = serde_json::json!(pattern);
            }
            print_tool(port_override, "harness", &args)
        }
        RosterAction::Check { platform, model } => {
            let mut args = serde_json::json!({ "action": "roster_check", "platform": platform });
            if let Some(model) = model {
                args["model"] = serde_json::json!(model);
            }
            print_tool(port_override, "harness", &args)
        }
    }
}

/// Exactly one of `--available` / `--unavailable` must be given.
fn resolve_state(available: bool, unavailable: bool) -> Result<&'static str> {
    match (available, unavailable) {
        (true, false) => Ok("available"),
        (false, true) => Ok("unavailable"),
        (true, true) => Err(anyhow!("Pass only one of --available or --unavailable.")),
        (false, false) => Err(anyhow!(
            "Pass one of --available or --unavailable to state the pair's state."
        )),
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
    use crate::tui::mcp_client::test_support::spawn_fake_daemon;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: RosterAction,
    }

    #[test]
    fn set_parses_unavailable_with_reason_and_until() {
        let cli = TestCli::try_parse_from([
            "test",
            "set",
            "opencode-go",
            "opencode-go/*",
            "--unavailable",
            "--reason",
            "quota",
            "--until",
            "2026-07-01T00:00:00Z",
        ])
        .expect("should parse");
        match cli.action {
            RosterAction::Set {
                platform,
                model_pattern,
                available,
                unavailable,
                reason,
                until,
            } => {
                assert_eq!(platform, "opencode-go");
                assert_eq!(model_pattern.as_deref(), Some("opencode-go/*"));
                assert!(!available);
                assert!(unavailable);
                assert_eq!(reason.as_deref(), Some("quota"));
                assert_eq!(until.as_deref(), Some("2026-07-01T00:00:00Z"));
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn set_rejects_both_state_flags() {
        let cli =
            TestCli::try_parse_from(["test", "set", "claude", "--available", "--unavailable"]);
        assert!(cli.is_err(), "conflicting flags must fail to parse");
    }

    #[test]
    fn resolve_state_requires_exactly_one() {
        assert_eq!(resolve_state(true, false).unwrap(), "available");
        assert_eq!(resolve_state(false, true).unwrap(), "unavailable");
        assert!(resolve_state(true, true).is_err());
        assert!(resolve_state(false, false).is_err());
    }

    #[test]
    fn list_parses_without_platform() {
        let cli = TestCli::try_parse_from(["test", "list"]).expect("should parse");
        assert!(matches!(cli.action, RosterAction::List { platform: None }));
    }

    #[tokio::test]
    async fn set_calls_harness_roster_set_with_state_string() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        let action = RosterAction::Set {
            platform: "opencode-go".to_string(),
            model_pattern: Some("opencode-go/*".to_string()),
            available: false,
            unavailable: true,
            reason: Some("quota".to_string()),
            until: Some("2026-07-01T00:00:00Z".to_string()),
        };
        handle_roster_action(action, Some(port)).await.unwrap();

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "harness");
        assert_eq!(calls[0]["arguments"]["action"], "roster_set");
        assert_eq!(calls[0]["arguments"]["platform"], "opencode-go");
        assert_eq!(calls[0]["arguments"]["model_pattern"], "opencode-go/*");
        assert_eq!(calls[0]["arguments"]["state"], "unavailable");
        assert_eq!(calls[0]["arguments"]["reason"], "quota");
        assert_eq!(calls[0]["arguments"]["until"], "2026-07-01T00:00:00Z");
    }

    #[tokio::test]
    async fn check_calls_harness_roster_check() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{\"available\":true}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        let action = RosterAction::Check {
            platform: "claude".to_string(),
            model: Some("opus".to_string()),
        };
        handle_roster_action(action, Some(port)).await.unwrap();

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "harness");
        assert_eq!(calls[0]["arguments"]["action"], "roster_check");
        assert_eq!(calls[0]["arguments"]["platform"], "claude");
        assert_eq!(calls[0]["arguments"]["model"], "opus");
    }
}
