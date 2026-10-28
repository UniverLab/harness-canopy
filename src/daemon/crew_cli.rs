//! CLI handlers for `canopy crew` subcommands.
//!
//! Every subcommand delegates to the MCP tool of the same purpose via
//! `daemon::cli_daemon::call_tool`, so the CLI and MCP share one code path.
//! Member editing is deliberately MCP-and-TUI-only: the CLI can only list,
//! show, create-from-ensemble and delete.

use anyhow::Result;
use clap::Subcommand;

use crate::daemon::cli_daemon::call_tool;

#[derive(Subcommand, Debug)]
pub(crate) enum CrewAction {
    /// List every crew with its version, members and binding counts.
    List,
    /// Show one crew and every binding of it.
    Show {
        /// The crew name.
        name: String,
    },
    /// Create a crew by seeding it from an ensemble's current members.
    Create {
        /// The new crew name.
        name: String,
        /// Optional description.
        #[arg(long)]
        description: Option<String>,
        /// The ensemble whose current members seed the crew.
        #[arg(long)]
        from_ensemble: String,
    },
    /// Delete a crew (refused while any ensemble, node or hook binds it).
    Delete {
        /// The crew name.
        name: String,
    },
}

pub async fn handle_crew_action(action: CrewAction, port_override: Option<u16>) -> Result<()> {
    match action {
        CrewAction::List => print_tool(
            port_override,
            "crew",
            &serde_json::json!({ "action": "list" }),
        ),
        CrewAction::Show { name } => print_tool(
            port_override,
            "crew",
            &serde_json::json!({ "action": "get", "name": name }),
        ),
        CrewAction::Create {
            name,
            description,
            from_ensemble,
        } => {
            let mut args = serde_json::json!({ "name": name, "from_ensemble": from_ensemble });
            if let Some(description) = description {
                args["description"] = serde_json::json!(description);
            }
            args["action"] = serde_json::json!("create");
            print_tool(port_override, "crew", &args)
        }
        CrewAction::Delete { name } => print_tool(
            port_override,
            "crew",
            &serde_json::json!({ "action": "delete", "name": name }),
        ),
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
        action: CrewAction,
    }

    #[test]
    fn create_parses_from_ensemble_and_description() {
        let cli = TestCli::try_parse_from([
            "test",
            "create",
            "implementers",
            "--description",
            "the implementer crew",
            "--from-ensemble",
            "ens-1",
        ])
        .expect("should parse");
        match cli.action {
            CrewAction::Create {
                name,
                description,
                from_ensemble,
            } => {
                assert_eq!(name, "implementers");
                assert_eq!(description.as_deref(), Some("the implementer crew"));
                assert_eq!(from_ensemble, "ens-1");
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn create_requires_from_ensemble() {
        let cli = TestCli::try_parse_from(["test", "create", "implementers"]);
        assert!(
            cli.is_err(),
            "create without --from-ensemble must not parse"
        );
    }

    #[tokio::test]
    async fn create_calls_crew_create_action_with_from_ensemble() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        let action = CrewAction::Create {
            name: "implementers".to_string(),
            description: None,
            from_ensemble: "ens-1".to_string(),
        };
        handle_crew_action(action, Some(port)).await.unwrap();

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "crew");
        assert_eq!(calls[0]["arguments"]["action"], "create");
        assert_eq!(calls[0]["arguments"]["name"], "implementers");
        assert_eq!(calls[0]["arguments"]["from_ensemble"], "ens-1");
    }

    #[tokio::test]
    async fn delete_calls_crew_delete_action() {
        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false
        }));
        let port: u16 = fake.port.parse().unwrap();
        let action = CrewAction::Delete {
            name: "implementers".to_string(),
        };
        handle_crew_action(action, Some(port)).await.unwrap();

        let calls = fake.recorded_calls();
        assert_eq!(calls[0]["name"], "crew");
        assert_eq!(calls[0]["arguments"]["action"], "delete");
        assert_eq!(calls[0]["arguments"]["name"], "implementers");
    }
}
