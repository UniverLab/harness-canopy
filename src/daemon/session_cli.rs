//! `canopy session` CLI subcommands.

use anyhow::Result;

#[derive(Debug, clap::Subcommand)]
pub(crate) enum SessionAction {
    /// Rename a live interactive session.
    Rename {
        /// Live session id or name.
        target: String,
        /// New session name (1-32 chars, lowercase a-z, digits, '-').
        new_name: String,
    },
}

pub(crate) fn handle_session_action(action: SessionAction, port: Option<u16>) -> Result<()> {
    match action {
        SessionAction::Rename { target, new_name } => handle_rename(port, &target, &new_name),
    }
}

fn handle_rename(port: Option<u16>, target: &str, new_name: &str) -> Result<()> {
    match crate::daemon::cli_daemon::call_tool(
        port,
        "session_rename",
        &serde_json::json!({ "target": target, "new_name": new_name }),
    ) {
        Ok(text) => print_rename_result(&text),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("could not reach the daemon on port") {
                return handle_rename_offline(target, new_name);
            }
            Err(err)
        }
    }
}

fn print_rename_result(text: &str) -> Result<()> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        let old = v.get("old_name").and_then(|v| v.as_str()).unwrap_or("?");
        let new = v.get("new_name").and_then(|v| v.as_str()).unwrap_or("?");
        println!("{old} -> {new}");
        return Ok(());
    }
    println!("{text}");
    Ok(())
}

fn handle_rename_offline(target: &str, new_name: &str) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("No home directory"))?;
    let canopy_dir = home.join(".canopy");
    let db_path = crate::domain::db_paths::database_path(&canopy_dir);
    let db = crate::db::Database::new(&db_path)?;
    let result = crate::sessions::rename::rename_live_session(&db, None, target, new_name)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("{} -> {}", result.old_name, result.new_name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SessionAction;
    use clap::{Parser, Subcommand};

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        Session {
            #[command(subcommand)]
            action: SessionAction,
        },
    }

    #[test]
    fn parses_session_rename() {
        let cli = Cli::try_parse_from(["canopy", "session", "rename", "alpha", "beta"]).unwrap();
        match cli.cmd {
            Cmd::Session {
                action: SessionAction::Rename { target, new_name },
            } => {
                assert_eq!(target, "alpha");
                assert_eq!(new_name, "beta");
            }
        }
    }
}
