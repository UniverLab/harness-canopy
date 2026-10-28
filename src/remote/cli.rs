//! CLI handlers for `canopy remote …` — the GitHub identity the relay uses
//! to admit only this owner's hosts, plus pairing, devices and the relay
//! switch.
//!
//! All operations go through [`super::auth`], [`super::pairing`] and
//! [`super::devices`] so the TUI (later specs) can call the same functions
//! without shelling out.

use anyhow::{bail, Result};
use clap::Subcommand;
use serde_json::json;

use super::auth::{self, RemoteEndpoints};
use super::devices::RemoteDevices;
use super::hostkey::HostKey;
use super::pairing::PairingState;
use super::qr;
use super::relay;
use crate::db::Database;
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
    /// Enable the relay connection (`[remote] enabled = true`).
    On,
    /// Disable the relay connection (`[remote] enabled = false`).
    Off,
    /// Create a one-time pairing code, its link and a terminal QR code.
    Pair,
    /// List paired browsers.
    Devices {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Revoke a paired browser by device id.
    Revoke {
        /// Device id (first 16 hex chars of SHA-256 over the browser key).
        device_id: String,
    },
}

/// Handle one `canopy remote` subcommand.
pub async fn handle_remote_action(action: RemoteAction) -> Result<()> {
    match action {
        RemoteAction::Login => handle_login().await,
        RemoteAction::Logout => handle_logout(),
        RemoteAction::Status { json } => handle_status(json),
        RemoteAction::On => handle_switch(true),
        RemoteAction::Off => handle_switch(false),
        RemoteAction::Pair => handle_pair(),
        RemoteAction::Devices { json } => handle_devices(json),
        RemoteAction::Revoke { device_id } => handle_revoke(&device_id),
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

fn handle_switch(enabled: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    crate::domain::settings::writer::write_keys(
        &data_dir,
        &[("remote.enabled", Some(toml_edit::Value::from(enabled)))],
    )?;
    println!("remote: {}", if enabled { "on" } else { "off" });
    Ok(())
}

fn handle_pair() -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let config = CanopyConfig::load(&data_dir);
    let host = HostKey::load_or_create(&data_dir)?;
    let state = PairingState::new(&data_dir);
    let created = state.create(&host.host_id(), chrono::Utc::now().timestamp())?;
    let link = relay::pair_link(&config.remote.relay_url, &created.code);
    let expires = chrono::DateTime::<chrono::Utc>::from_timestamp(created.expires_at, 0)
        .map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| created.expires_at.to_string());
    println!("Pairing code: {}", created.code);
    println!("Link: {link}");
    println!("Expires: {expires}");
    print!("{}", qr::render(&link)?);
    Ok(())
}

fn handle_devices(json_output: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let devices = open_devices(&data_dir)?;
    let rows = devices.list()?;
    if json_output {
        let payload: Vec<_> = rows
            .iter()
            .map(|device| {
                json!({
                    "id": device.id,
                    "label": device.label,
                    "paired_at": device.paired_at,
                    "last_seen": device.last_seen,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no paired devices");
        return Ok(());
    }
    for device in rows {
        println!(
            "{}  {}  paired {}  last seen {}",
            device.id,
            device.label,
            device.paired_at,
            device.last_seen.as_deref().unwrap_or("never")
        );
    }
    Ok(())
}

fn handle_revoke(device_id: &str) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = open_db(&data_dir)?;
    let devices = RemoteDevices::new(db.clone());
    if !devices.revoke(device_id)? {
        bail!("no paired device with id {device_id}");
    }
    // The daemon reads this queue on its next tick and closes the device's
    // live connection at once.
    relay::queue_revocation(&db, device_id);
    println!("revoked {device_id}");
    Ok(())
}

fn open_db(data_dir: &std::path::Path) -> Result<Database> {
    Database::new(&crate::domain::db_paths::database_path(data_dir))
}

fn open_devices(data_dir: &std::path::Path) -> Result<RemoteDevices> {
    Ok(RemoteDevices::new(open_db(data_dir)?))
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

    #[test]
    fn on_off_pair_parse() {
        assert!(matches!(
            TestCli::try_parse_from(["test", "on"]).expect("on").action,
            RemoteAction::On
        ));
        assert!(matches!(
            TestCli::try_parse_from(["test", "off"])
                .expect("off")
                .action,
            RemoteAction::Off
        ));
        assert!(matches!(
            TestCli::try_parse_from(["test", "pair"])
                .expect("pair")
                .action,
            RemoteAction::Pair
        ));
    }

    #[test]
    fn devices_and_revoke_parse() {
        let cli = TestCli::try_parse_from(["test", "devices", "--json"]).expect("devices");
        assert!(matches!(cli.action, RemoteAction::Devices { json: true }));
        let cli = TestCli::try_parse_from(["test", "revoke", "bfcba2b742a807bb"]).expect("revoke");
        match cli.action {
            RemoteAction::Revoke { device_id } => assert_eq!(device_id, "bfcba2b742a807bb"),
            _ => panic!("expected revoke"),
        }
    }
}
