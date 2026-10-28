//! The Remote panel's data: everything `canopy remote` shows, as JSON the
//! daemon serves and the TUI renders.
//!
//! The same functions serve the CLI today and the hub's Remote tab: the
//! status view (sign-in, the relay switch, the relay phase, the live and
//! paired devices), pairing-code creation, the on/off switch and revokes.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::live::{RelayPhase, RemoteLive};
use crate::db::Database;

/// One paired device as the panel lists it.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DeviceView {
    /// First 16 hex chars of SHA-256 over the device key.
    pub id: String,
    /// Human label, or `browser`.
    pub label: String,
    /// RFC 3339 of the last live connection, if any.
    pub last_seen: Option<String>,
    /// Whether the device has a live peer right now.
    pub live: bool,
}

/// Everything the Remote tab shows.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteView {
    /// Whether a GitHub identity is stored.
    pub signed_in: bool,
    /// GitHub login when signed in, `None` otherwise.
    pub login: Option<String>,
    /// Whether `[remote] enabled` is on.
    pub enabled: bool,
    /// The configured relay URL.
    pub relay_url: String,
    /// One of `connected`, `connecting`, `token rejected: sign in again`,
    /// `off`.
    pub relay: String,
    /// Ids of the devices with a live peer.
    pub live_devices: Vec<String>,
    /// Every paired device, oldest pairing first.
    pub devices: Vec<DeviceView>,
}

/// A fresh pairing code with its link, expiry and QR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairView {
    /// Formatted as `XXXXX-XXXXX`.
    pub code: String,
    /// `<relay_url>/pair#<code>`.
    pub link: String,
    /// Unix seconds when the code stops working.
    pub expires_at: i64,
    /// The link as a QR code in Unicode half blocks.
    pub qr: String,
}

/// The relay row's text for one phase.
pub fn relay_text(phase: RelayPhase) -> &'static str {
    match phase {
        RelayPhase::Connected => "connected",
        RelayPhase::Connecting => "connecting",
        RelayPhase::TokenRejected => "token rejected: sign in again",
        RelayPhase::Off => "off",
    }
}

/// Build the status view from config, the stored identity, the live relay
/// state and the device registry.
pub fn status_view(data_dir: &Path, db: &Database, live: &RemoteLive) -> Result<RemoteView> {
    let config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let identity = super::auth::load_identity(data_dir);
    let status = super::auth::build_remote_status(&config, identity.as_ref());
    let live_ids = live.live_devices();
    let rows = super::devices::RemoteDevices::new(db.clone()).list()?;
    let devices = rows
        .into_iter()
        .map(|device| DeviceView {
            live: live_ids.contains(&device.id),
            id: device.id,
            label: device.label,
            last_seen: device.last_seen,
        })
        .collect();
    Ok(RemoteView {
        signed_in: status.signed_in,
        login: status.login,
        enabled: status.enabled,
        relay_url: status.relay_url,
        relay: relay_text(live.phase()).to_string(),
        live_devices: live_ids,
        devices,
    })
}

/// Create a one-time pairing code with its link and QR, replacing any live
/// one — the same code `canopy remote pair` prints.
pub fn create_pairing(data_dir: &Path) -> Result<PairView> {
    let config = crate::domain::canopy_config::CanopyConfig::load(data_dir);
    let host = super::hostkey::HostKey::load_or_create(data_dir)?;
    let state = super::pairing::PairingState::new(data_dir);
    let created = state.create(&host.host_id(), chrono::Utc::now().timestamp())?;
    let link = super::relay::pair_link(&config.remote.relay_url, &created.code);
    let qr = super::qr::render_level(&link, qrcode::EcLevel::L)?;
    Ok(PairView {
        code: created.code,
        link,
        expires_at: created.expires_at,
        qr,
    })
}

/// Turn the relay connection on or off (`[remote] enabled`); the daemon
/// picks the change up without a restart.
pub fn set_enabled(data_dir: &Path, enabled: bool) -> Result<()> {
    crate::domain::settings::writer::write_keys(
        data_dir,
        &[("remote.enabled", Some(toml_edit::Value::from(enabled)))],
    )?;
    Ok(())
}

/// Revoke a device: delete the row and queue the id so the daemon closes
/// its live connection at once. Returns whether a row was removed.
pub fn revoke(db: &Database, device_id: &str) -> Result<bool> {
    let removed = super::devices::RemoteDevices::new(db.clone()).revoke(device_id)?;
    if removed {
        super::relay::queue_revocation(db, device_id);
    }
    Ok(removed)
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
