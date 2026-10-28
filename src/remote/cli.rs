//! CLI handlers for `canopy remote login|logout|status` — the GitHub
//! identity the relay uses to admit only this owner's hosts.
//!
//! All operations go through [`super::auth`] so the TUI (next specs) can
//! call the same functions without shelling out.

use anyhow::Result;
use clap::Subcommand;

use super::auth::{self, RemoteEndpoints};
use crate::domain::canopy_config::CanopyConfig;

#[derive(Subcommand, Debug)]
pub enum RemoteAction {
    /// Sign in to GitHub with the OAuth device flow (identity only, no scopes).
    Login,
    /// Delete the stored GitHub identity.
    Logout,
    /// Show the sign-in state, whether remote is enabled, and the relay URL.
    Status {
        /// Print `{signed_in, login, enabled, relay_url}` as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Handle one `canopy remote` subcommand.
pub async fn handle_remote_action(action: RemoteAction) -> Result<()> {
    match action {
        RemoteAction::Login => handle_login().await,
        RemoteAction::Logout => handle_logout(),
        RemoteAction::Status { json } => handle_status(json),
    }
}

async fn handle_login() -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let config = CanopyConfig::load(&data_dir);
    let identity = auth::login(
        &config.remote.github_client_id,
        &data_dir,
        &RemoteEndpoints::default(),
        |info| {
            println!(
                "Open https://github.com/login/device and enter {}",
                info.user_code
            );
            if let Some(uri) = &info.verification_uri_complete {
                println!("{uri}");
            }
        },
    )
    .await?;
    println!("Signed in to GitHub as {}.", identity.login);
    Ok(())
}

fn handle_logout() -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    auth::logout(&data_dir)?;
    println!("Signed out of GitHub.");
    Ok(())
}

fn handle_status(json: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let config = CanopyConfig::load(&data_dir);
    let identity = auth::load_identity(&data_dir);
    let status = auth::build_remote_status(&config, identity.as_ref());
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
        return Ok(());
    }
    match &status.login {
        Some(login) => println!("login: {login}"),
        None => println!("not signed in"),
    }
    println!("enabled: {}", status.enabled);
    println!("relay_url: {}", status.relay_url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: RemoteAction,
    }

    #[test]
    fn login_parses() {
        let cli = TestCli::try_parse_from(["test", "login"]).expect("should parse");
        assert!(matches!(cli.action, RemoteAction::Login));
    }

    #[test]
    fn logout_parses() {
        let cli = TestCli::try_parse_from(["test", "logout"]).expect("should parse");
        assert!(matches!(cli.action, RemoteAction::Logout));
    }

    #[test]
    fn status_parses_with_and_without_json_flag() {
        let cli = TestCli::try_parse_from(["test", "status"]).expect("should parse");
        assert!(matches!(cli.action, RemoteAction::Status { json: false }));

        let cli = TestCli::try_parse_from(["test", "status", "--json"]).expect("should parse");
        assert!(matches!(cli.action, RemoteAction::Status { json: true }));
    }
}
