//! CLI handlers for `canopy queue` subcommands.
//!
//! Like `spec_cli`, every state-changing subcommand delegates to the daemon's
//! MCP tool of the same purpose (`queue` `set_group`) via
//! `daemon::cli_daemon::call_tool` rather than writing to the database
//! directly, so the CLI and MCP paths are one code path.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;

#[derive(Subcommand)]
pub enum QueueAction {
    /// Set or clear the context group of an existing member of a queue.
    Group {
        /// Queue ID (or unique prefix).
        queue_id: String,
        /// Spec ID (or unique prefix).
        spec_id: String,
        /// Context group name to assign.
        group: Option<String>,
        /// Clear the member's group (ungroup it).
        #[arg(long)]
        clear: bool,
    },
}

pub async fn handle_queue_action(action: QueueAction, port_override: Option<u16>) -> Result<()> {
    match action {
        QueueAction::Group {
            queue_id,
            spec_id,
            group,
            clear,
        } => set_group(port_override, &queue_id, &spec_id, group.as_deref(), clear),
    }
}

fn set_group(
    port_override: Option<u16>,
    queue_id: &str,
    spec_id: &str,
    group: Option<&str>,
    clear: bool,
) -> Result<()> {
    let group_value = match (group, clear) {
        (Some(_), true) => {
            return Err(anyhow!("Provide either a group name or --clear, not both."))
        }
        (None, false) => {
            return Err(anyhow!(
                "Provide a group name, or pass --clear to ungroup the member."
            ))
        }
        (Some(name), false) => serde_json::json!(name),
        (None, true) => serde_json::Value::Null,
    };
    let text = call_tool(
        port_override,
        "queue",
        &serde_json::json!({
            "action": "set_group",
            "queue_id": queue_id,
            "spec_id": spec_id,
            "group": group_value,
        }),
    )?;
    println!("{text}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::mcp_client::test_support::{spawn_fake_daemon, unused_port};
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: QueueAction,
    }

    #[test]
    fn queue_group_parses_group_name() {
        let cli = TestCli::try_parse_from(["test", "group", "q1", "s1", "g1"]).expect("parses");
        match cli.action {
            QueueAction::Group {
                queue_id,
                spec_id,
                group,
                clear,
            } => {
                assert_eq!(queue_id, "q1");
                assert_eq!(spec_id, "s1");
                assert_eq!(group.as_deref(), Some("g1"));
                assert!(!clear);
            }
        }
    }

    #[test]
    fn queue_group_parses_clear_flag() {
        let cli =
            TestCli::try_parse_from(["test", "group", "q1", "s1", "--clear"]).expect("parses");
        match cli.action {
            QueueAction::Group {
                queue_id,
                spec_id,
                group,
                clear,
            } => {
                assert_eq!(queue_id, "q1");
                assert_eq!(spec_id, "s1");
                assert_eq!(group, None);
                assert!(clear);
            }
        }
    }

    #[test]
    fn set_group_sends_group_string() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{\"queue\":{\"id\":\"q1\",\"members\":[]}}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        set_group(Some(port), "q1", "s1", Some("g1"), false).expect("should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "queue");
        assert_eq!(calls[0]["arguments"]["action"], "set_group");
        assert_eq!(calls[0]["arguments"]["queue_id"], "q1");
        assert_eq!(calls[0]["arguments"]["spec_id"], "s1");
        assert_eq!(calls[0]["arguments"]["group"], "g1");
    }

    #[test]
    fn set_group_clear_sends_null() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "{\"queue\":{\"id\":\"q1\",\"members\":[]}}"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();

        set_group(Some(port), "q1", "s1", None, true).expect("should succeed");

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "queue");
        assert_eq!(calls[0]["arguments"]["action"], "set_group");
        assert_eq!(calls[0]["arguments"]["group"], serde_json::Value::Null);
    }

    #[test]
    fn set_group_rejects_group_and_clear_together() {
        let err = set_group(Some(1), "q1", "s1", Some("g1"), true).unwrap_err();
        assert!(err.to_string().contains("not both"), "{err}");
    }

    #[test]
    fn set_group_rejects_neither_group_nor_clear() {
        let err = set_group(Some(1), "q1", "s1", None, false).unwrap_err();
        assert!(err.to_string().contains("--clear"), "{err}");
    }

    /// The daemon-unreachable path must read distinctly from a tool-level
    /// rejection — this is the CLI's whole reason for being.
    #[test]
    fn set_group_reports_daemon_unreachable() {
        for _ in 0..5 {
            let port: u16 = unused_port().parse().unwrap();
            match set_group(Some(port), "q1", "s1", Some("g1"), false) {
                Err(err) => {
                    let chain: Vec<String> = err.chain().map(ToString::to_string).collect();
                    assert!(chain
                        .iter()
                        .any(|m| m.contains("could not reach the daemon")));
                    return;
                }
                Ok(()) => continue,
            }
        }
        panic!("port kept getting claimed by another test after 5 attempts");
    }
}
