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
        /// Print `{signed_in, login, enabled, relay_url, last_error,
        /// last_reconnect, config_fault?}` as JSON.
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
    /// Disconnect a paired browser by device id, keeping the pairing.
    Disconnect {
        /// Device id (first 16 hex chars of SHA-256 over the browser key).
        device_id: String,
    },
    /// Issue a single-use guest invite code for a GitHub login (printed once,
    /// stored as a hash only).
    Invite {
        /// GitHub login the invite is issued to.
        login: String,
        /// Days the invite stays live.
        #[arg(long, default_value_t = super::invite::DEFAULT_DAYS)]
        days: i64,
    },
    /// List guest invites with expiry and used state.
    Invites {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Revoke a guest invite by code, or every live invite by login.
    Uninvite {
        /// Invite code (`CNR-…`) or GitHub login.
        target: String,
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
        RemoteAction::Disconnect { device_id } => handle_disconnect(&device_id),
        RemoteAction::Invite { login, days } => handle_invite(&login, days).await,
        RemoteAction::Invites { json } => handle_invites(json),
        RemoteAction::Uninvite { target } => handle_uninvite(&target),
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
    let mut config = CanopyConfig::load(&data_dir);
    let (enabled, relay_url) =
        crate::domain::config_cache::effective_status_remote(&data_dir, &config);
    config.remote.enabled = enabled;
    config.remote.relay_url = relay_url;
    let identity = auth::load_identity(&data_dir);
    let db = open_db(&data_dir).ok();
    let mut status = auth::build_remote_status(&config, identity.as_ref(), db.as_ref());
    status.config_fault = crate::domain::config_cache::read_fault_message(&data_dir);
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
    if let Some(at) = &status.last_reconnect {
        println!("last_reconnect: {at}");
    }
    if let Some(error) = &status.last_error {
        println!("last_error: {error}");
    }
    if let Some(fault) = &status.config_fault {
        println!("{fault}");
    }
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
    // The daemon reads the queued id on its next tick and closes the
    // device's live connection at once.
    if !super::panel::revoke(&db, device_id)? {
        bail!("no paired device with id {device_id}");
    }
    println!("revoked {device_id}");
    Ok(())
}

fn handle_disconnect(device_id: &str) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = open_db(&data_dir)?;
    // The daemon reads the queued id on its next tick and closes the
    // device's live peer at once; the pairing stays, so it may reconnect.
    if !super::panel::disconnect(&db, device_id)? {
        bail!("no paired device with id {device_id}");
    }
    println!("disconnected {device_id}");
    Ok(())
}

async fn handle_invite(login: &str, days: i64) -> Result<()> {
    if days < 1 {
        bail!("invite lifetime must be at least 1 day");
    }
    let data_dir = crate::ensure_data_dir()?;
    let Some(identity) = auth::load_identity(&data_dir) else {
        bail!("not signed in: run canopy remote login first");
    };
    let login = super::trust::normalize_login(login);
    if login.is_empty() {
        bail!("invite login must not be blank");
    }
    let Some(uid) = super::guest_allow::resolve_one_login(
        &login,
        &identity.token,
        &super::guest_allow::github_api(),
    )
    .await
    else {
        bail!("could not resolve the GitHub login {login}");
    };
    let db = open_db(&data_dir)?;
    let issued = super::invite::issue(&db, &login, uid, chrono::Utc::now().timestamp(), days)?;
    // The link names the owner's room (this identity's uid) and this host;
    // a host that never paired or connected has no key, so no link yet.
    let config = CanopyConfig::load(&data_dir);
    let host_id = HostKey::load(&data_dir)?.map(|key| key.host_id());
    let link = super::invite::guest_link_for(
        &config.remote.relay_url,
        identity.id,
        host_id.as_deref(),
        &issued.code,
    );
    // The one and only cleartext sighting: printed, never logged.
    println!("Invite code for {login}: {}", issued.code);
    match &link {
        Ok(link) => println!("Guest link: {link}"),
        Err(reason) => println!("{}", reason.hint()),
    }
    println!("Expires: {}", format_unix(issued.expires_at));
    if link.is_ok() {
        println!("Send {login} the link: it works once, after {login} signs in with GitHub.");
    }
    if !config.remote.enabled {
        println!("Note: remote is off; the guest cannot connect until `canopy remote on`.");
    }
    if !db
        .list_trust()?
        .iter()
        .any(|profile| profile.login == login)
    {
        println!(
            "Note: {login} holds no trust grant, so the host refuses the guest: add one from \
             the TUI Remote tab (t) or with the remote tool's trust_add."
        );
    }
    Ok(())
}

fn handle_invites(json_output: bool) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let rows = open_db(&data_dir)?.list_invites()?;
    if json_output {
        let payload: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "login": row.login,
                    "uid": row.uid,
                    "expires_at": row.expires_at,
                    "used": row.used,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no invites");
        return Ok(());
    }
    let now = chrono::Utc::now().timestamp();
    for row in rows {
        let state = if row.used {
            "used"
        } else if row.expires_at <= now {
            "expired"
        } else {
            "live"
        };
        println!(
            "{}  expires {}  {state}",
            row.login,
            format_unix(row.expires_at)
        );
    }
    Ok(())
}

fn handle_uninvite(target: &str) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = open_db(&data_dir)?;
    let now = chrono::Utc::now().timestamp();
    if let Some(canonical) = super::invite::normalize_code_input(target) {
        let hash = super::invite::code_hash(&canonical);
        if db.revoke_invite(&hash)? {
            println!("revoked invite {canonical}");
            return Ok(());
        }
        // A code-shaped login falls through to the login path below.
    }
    let removed = db.revoke_invites_for_login(target, now)?;
    if removed == 0 {
        bail!("no live invite for {target}");
    }
    let login = super::trust::normalize_login(target);
    if removed == 1 {
        println!("revoked 1 invite for {login}");
    } else {
        println!("revoked {removed} invites for {login}");
    }
    Ok(())
}

/// One Unix timestamp as `YYYY-MM-DD HH:MM:SS UTC`, or the raw number when
/// out of range.
fn format_unix(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn open_db(data_dir: &std::path::Path) -> Result<Database> {
    Database::new_safe(&crate::domain::db_paths::database_path(data_dir), data_dir)
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
        let cli = TestCli::try_parse_from(["test", "disconnect", "bfcba2b742a807bb"])
            .expect("disconnect");
        match cli.action {
            RemoteAction::Disconnect { device_id } => assert_eq!(device_id, "bfcba2b742a807bb"),
            _ => panic!("expected disconnect"),
        }
    }

    #[test]
    fn invite_invites_and_uninvite_parse() {
        let cli = TestCli::try_parse_from(["test", "invite", "octocat"]).expect("invite");
        match cli.action {
            RemoteAction::Invite { login, days } => {
                assert_eq!(login, "octocat");
                assert_eq!(days, 7);
            }
            _ => panic!("expected invite"),
        }
        let cli = TestCli::try_parse_from(["test", "invite", "octocat", "--days", "1"])
            .expect("invite with days");
        assert!(matches!(cli.action, RemoteAction::Invite { days: 1, .. }));
        let cli = TestCli::try_parse_from(["test", "invites", "--json"]).expect("invites");
        assert!(matches!(cli.action, RemoteAction::Invites { json: true }));
        let cli = TestCli::try_parse_from(["test", "uninvite", "octocat"]).expect("uninvite");
        match cli.action {
            RemoteAction::Uninvite { target } => assert_eq!(target, "octocat"),
            _ => panic!("expected uninvite"),
        }
    }
}
