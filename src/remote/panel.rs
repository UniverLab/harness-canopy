//! The Remote panel's data: everything `canopy remote` shows, as JSON the
//! daemon serves and the TUI renders.
//!
//! The same functions serve the CLI today and the hub's Remote tab: the
//! status view (sign-in, the relay switch, the relay phase, the live and
//! paired devices), pairing-code creation, the on/off switch and revokes.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::live::{RelayPhase, RemoteLive};
use super::trust;
use crate::db::Database;

/// One paired device as the panel lists it.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct DeviceView {
    /// First 16 hex chars of SHA-256 over the device key.
    pub id: String,
    /// Human label, or `browser`.
    pub label: String,
    /// RFC 3339 of the pairing.
    #[serde(default)]
    pub paired_at: String,
    /// RFC 3339 of the last live connection, if any.
    pub last_seen: Option<String>,
    /// Whether the device has a live peer right now.
    pub live: bool,
}

/// One trusted GitHub profile as the panel lists it.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TrustedView {
    /// Lowercased GitHub login.
    pub login: String,
    /// Whether the profile grants every session.
    pub all_sessions: bool,
    /// Granted session names, ascending. Empty when `all_sessions`.
    pub sessions: Vec<String>,
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
    /// Every trusted guest profile, ascending by login.
    #[serde(default)]
    pub trusted: Vec<TrustedView>,
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
            paired_at: device.paired_at,
            last_seen: device.last_seen,
        })
        .collect();
    let trusted = db
        .list_trust()?
        .into_iter()
        .map(|profile| TrustedView {
            login: profile.login,
            all_sessions: profile.all_sessions,
            sessions: profile.sessions,
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
        trusted,
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

/// Disconnect a device: queue the id so the daemon closes its live peer at
/// once, keeping the pairing so the device may reconnect. Returns whether
/// the device is paired (an unknown id queues nothing).
pub fn disconnect(db: &Database, device_id: &str) -> Result<bool> {
    if !super::devices::RemoteDevices::new(db.clone()).is_paired(device_id)? {
        return Ok(false);
    }
    super::relay::queue_disconnect(db, device_id);
    Ok(true)
}

/// Trust a GitHub profile with every session or a named set. A narrowed
/// grant kicks the login's live guest peers at once (they reconnect and
/// re-resolve); a widened or fresh grant kicks nothing. Returns the stored
/// profile.
pub fn trust_add(
    db: &Database,
    login: &str,
    all_sessions: bool,
    sessions: &[String],
) -> Result<TrustedView> {
    let login = trust::normalize_login(login);
    anyhow::ensure!(!login.is_empty(), "login must not be empty");
    let previous = db.get_trust(&login)?;
    db.upsert_trust(&login, all_sessions)?;
    if all_sessions {
        db.set_grants(&login, &[])?;
    } else {
        db.set_grants(&login, sessions)?;
    }
    if narrowed(&previous, all_sessions, sessions) {
        super::relay::queue_trust_revocation(db, &login);
    }
    let stored = db
        .get_trust(&login)?
        .ok_or_else(|| anyhow::anyhow!("trust row vanished after its own write"))?;
    Ok(TrustedView {
        login: stored.login,
        all_sessions: stored.all_sessions,
        sessions: stored.sessions,
    })
}

/// Whether a rewrite shrinks what `previous` granted: all-to-scoped, or a
/// scoped set that drops a name (compared trimmed, like the store keeps
/// them). Fresh and widened grants never narrow.
fn narrowed(
    previous: &Option<crate::db::remote_trust::TrustedProfile>,
    all_sessions: bool,
    sessions: &[String],
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    if previous.all_sessions {
        return !all_sessions;
    }
    if all_sessions {
        return false;
    }
    let next: HashSet<String> = sessions
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    previous.sessions.iter().any(|name| !next.contains(name))
}

/// Forget a GitHub profile and kick its live guest peers at once. Returns
/// whether a row was removed (an unknown login queues nothing).
pub fn trust_remove(db: &Database, login: &str) -> Result<bool> {
    let removed = db.remove_trust(login)?;
    if removed {
        super::relay::queue_trust_revocation(db, login);
    }
    Ok(removed)
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
