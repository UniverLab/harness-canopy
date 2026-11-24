//! The daemon-level reaper: closes live remote peers without the relay.
//!
//! The only code that used to close peers on a revocation was the socket
//! tick inside `serve` (`relay_socket`), so a revoked device kept its
//! WebRTC channel whenever the supervisor was waiting (remote off, signed
//! out) or reconnecting. This task ticks every second for the life of the
//! daemon and never touches the socket, so it runs in every supervisor
//! state: it drains the revocation queues, closes everything while remote
//! is off or nobody is signed in, and sweeps owner peers whose device row
//! is gone without any queue entry.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::application::ports::StateRepository as _;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::auth;
use crate::remote::connection::DevicePeers;
use crate::remote::devices::RemoteDevices;
use crate::remote::relay::{
    ConnectionRegistry, REMOTE_DISCONNECT_KEY, REMOTE_REVOKE_KEY, REMOTE_TRUST_REVOKE_KEY,
};
use crate::remote::trust;

/// Drain the three revocation queues (`remote_revoke`, `remote_disconnect`,
/// `remote_trust_revoke`), closing the live peers they name. A revoked peer
/// hears `error{code:"revoked"}` before its close; a disconnected one just
/// closes and may reconnect. Returns how many live peers closed.
pub fn drain_queues(db: &Database, peers: &DevicePeers, connections: &ConnectionRegistry) -> usize {
    let live: HashSet<String> = peers.live_ids().into_iter().collect();
    let mut closed = 0;
    for id in drain_queue(db, REMOTE_REVOKE_KEY, |id| {
        peers.close_revoked(id);
        connections.close(id);
    }) {
        if live.contains(&id) {
            closed += 1;
        }
    }
    for id in drain_queue(db, REMOTE_DISCONNECT_KEY, |id| {
        connections.close(id);
        peers.close(id);
    }) {
        if live.contains(&id) {
            closed += 1;
        }
    }
    for login in drain_queue(db, REMOTE_TRUST_REVOKE_KEY, |_| {}) {
        closed += close_login_peers(db, peers, connections, &login);
    }
    closed
}

/// Close every live peer at once, owner and guest, with no farewell frame.
/// The pairing stays, so the devices may reconnect.
pub fn close_all(peers: &DevicePeers, connections: &ConnectionRegistry) -> usize {
    let ids = peers.live_ids();
    for id in &ids {
        connections.close(id);
        peers.close(id);
    }
    ids.len()
}

/// The reaper loop: one pass a second until the daemon exits. Queued
/// revocations close at once; `remote off` and a missing identity close
/// everything; every fifth pass closes owner peers whose device row is gone
/// (a direct database change leaves no queue entry).
pub async fn run(
    data_dir: PathBuf,
    db: Database,
    peers: DevicePeers,
    connections: ConnectionRegistry,
) {
    let mut tick = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        tick += 1;
        let queued = drain_queues(&db, &peers, &connections);
        if queued > 0 {
            tracing::info!("remote: closed {queued} live peer(s): queued");
        }
        close_when_host_dark(&data_dir, &peers, &connections);
        if tick.is_multiple_of(5) {
            close_removed_with_log(&db, &peers, &connections);
        }
    }
}

/// Close everything while `[remote] enabled` is false (`remote off`) or no
/// GitHub identity is stored (`signed out`), logging the pass when it
/// closes anything.
fn close_when_host_dark(data_dir: &Path, peers: &DevicePeers, connections: &ConnectionRegistry) {
    let reason = if !CanopyConfig::load(data_dir).remote.enabled {
        "remote off"
    } else if auth::load_identity(data_dir).is_none() {
        "signed out"
    } else {
        return;
    };
    let n = close_all(peers, connections);
    if n > 0 {
        tracing::info!("remote: closed {n} live peer(s): {reason}");
    }
}

/// Close owner peers whose device row is gone, logging the pass when it
/// closes anything.
fn close_removed_with_log(db: &Database, peers: &DevicePeers, connections: &ConnectionRegistry) {
    let n = close_removed_devices(db, peers, connections);
    if n > 0 {
        tracing::info!("remote: closed {n} live peer(s): device removed");
    }
}

/// Close every live peer paired under one login. A narrowed grant closes
/// the whole peer — there is no live-push channel for a smaller grant, so
/// the guest reconnects and re-resolves it. Returns how many live peers
/// closed.
fn close_login_peers(
    db: &Database,
    peers: &DevicePeers,
    connections: &ConnectionRegistry,
    login: &str,
) -> usize {
    let login = trust::normalize_login(login);
    let Ok(devices) = RemoteDevices::new(db.clone()).list() else {
        return 0;
    };
    let mut closed = 0;
    for device in devices {
        if trust::normalize_login(&device.github_login) == login {
            if peers.live_ids().iter().any(|id| id == &device.id) {
                closed += 1;
            }
            peers.close_revoked(&device.id);
            connections.close(&device.id);
        }
    }
    closed
}

/// Close every live owner peer whose device row is gone. Guest peers
/// (`guest-{uid}`) have no device row and are never touched here.
fn close_removed_devices(
    db: &Database,
    peers: &DevicePeers,
    connections: &ConnectionRegistry,
) -> usize {
    let devices = RemoteDevices::new(db.clone());
    let mut closed = 0;
    for id in peers.live_ids() {
        if id.starts_with("guest-") {
            continue;
        }
        let gone = devices.get(&id).ok().flatten().is_none();
        if gone {
            peers.close_revoked(&id);
            connections.close(&id);
            closed += 1;
        }
    }
    closed
}

/// Close every id queued under `key`, returning the drained ids. Ids that
/// land while closing stay queued for the next tick.
fn drain_queue(db: &Database, key: &str, mut close: impl FnMut(&str)) -> Vec<String> {
    let Ok(Some(value)) = db.get_state(key) else {
        return Vec::new();
    };
    let drained: Vec<String> = value
        .split(',')
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect();
    for id in &drained {
        close(id);
    }
    // An id that lands while we close must survive: keep whatever we did
    // not just drain instead of blanking the key.
    let current = db.get_state(key).ok().flatten().unwrap_or_default();
    let remaining: Vec<&str> = current
        .split(',')
        .filter(|id| !id.is_empty() && !drained.iter().any(|drained| drained == *id))
        .collect();
    let _ = db.set_state(key, &remaining.join(","));
    drained
}

#[cfg(test)]
#[path = "reaper_tests.rs"]
mod tests;
