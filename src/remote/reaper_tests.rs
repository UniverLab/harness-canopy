//! The daemon-level reaper closes live peers without the relay socket:
//! queued revocations land on the next one-second tick, `remote off` and a
//! missing identity close everything, and an owner peer whose device row
//! vanished closes on every fifth tick. Guest peers (`guest-{uid}`) have no
//! row and are never swept as removed.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{close_all, drain_queues, run};
use crate::daemon::handler::HandlerDeps;
use crate::daemon::TaskTriggerHandler;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::auth::{self, GitHubIdentity};
use crate::remote::connection::DevicePeers;
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::HostKey;
use crate::remote::relay::{queue_revocation, ConnectionRegistry};
use crate::remote::signaling::{verify_offer, Refusal};

/// One scratch dir holding both the config/identity files and the database.
fn scratch() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    (dir, db)
}

fn test_config(dir: &tempfile::TempDir, enabled: bool) {
    let mut config = CanopyConfig::default();
    config.remote.enabled = enabled;
    config.save_for_tests(dir.path()).expect("config");
}

fn test_identity(dir: &tempfile::TempDir) {
    auth::save_identity(
        dir.path(),
        &GitHubIdentity {
            token: "test-token".to_string(),
            login: "octocat".to_string(),
            id: 1,
        },
    )
    .expect("identity");
}

/// Register a live peer for `id` and return its token, cancelled on close.
fn live_peer(peers: &DevicePeers, id: &str) -> Arc<CancellationToken> {
    let token = Arc::new(CancellationToken::new());
    let (control, _inbox) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        peers.register(id, &format!("d:{id}"), Arc::clone(&token), control),
        "a fresh peer registers"
    );
    token
}

/// Pair one owner device and return its id.
fn pair_owned(db: &Database, label: &str) -> String {
    RemoteDevices::new(db.clone())
        .pair(
            format!("spki for {label}").into_bytes(),
            label,
            "octocat",
            "2026-01-01T00:00:00Z",
        )
        .expect("pair")
        .id
}

/// Wait until `token` cancels, failing after `timeout`.
async fn wait_cancelled(token: &Arc<CancellationToken>, timeout: Duration, what: &str) {
    let start = tokio::time::Instant::now();
    while !token.is_cancelled() {
        assert!(
            start.elapsed() < timeout,
            "{what} did not close its live peer in time"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A queued revocation closes the live peer on the next tick, and the peer
/// hears the revoked frame — with no relay and no supervisor running.
#[tokio::test]
async fn queued_revocation_closes_the_live_peer_with_a_revoked_farewell() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    let id = pair_owned(&db, "queued laptop");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let token = live_peer(&peers, &id);
    queue_revocation(&db, &id);
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    wait_cancelled(&token, Duration::from_millis(1500), "a queued revocation").await;
    reaper.abort();
    assert!(
        peers.take_revoked(&id),
        "a revoked peer hears the revoked frame first"
    );
}

/// `remote off` closes owner and guest peers on the next tick, with no
/// revoked mark — the pairing stays, so the devices may reconnect. The
/// config flips after the reaper starts, proving it re-reads every tick.
#[tokio::test]
async fn remote_off_closes_owner_and_guest_without_a_revoked_mark() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    let owner = pair_owned(&db, "off laptop");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let owner_token = live_peer(&peers, &owner);
    let guest_token = live_peer(&peers, "guest-4242");
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    test_config(&dir, false);
    wait_cancelled(&owner_token, Duration::from_secs(2), "remote off").await;
    wait_cancelled(&guest_token, Duration::from_secs(2), "remote off").await;
    reaper.abort();
    assert!(
        !peers.take_revoked(&owner) && !peers.take_revoked("guest-4242"),
        "switching off is not a revocation: no revoked mark"
    );
}

/// A deleted identity closes owner and guest peers the same way. The
/// identity is removed after the reaper starts, proving it re-reads it
/// every tick.
#[tokio::test]
async fn signed_out_closes_owner_and_guest_without_a_revoked_mark() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    let owner = pair_owned(&db, "logout laptop");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let owner_token = live_peer(&peers, &owner);
    let guest_token = live_peer(&peers, "guest-5150");
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    auth::logout(dir.path()).expect("the identity file is removed");
    wait_cancelled(&owner_token, Duration::from_secs(2), "a missing identity").await;
    wait_cancelled(&guest_token, Duration::from_secs(2), "a missing identity").await;
    reaper.abort();
    assert!(
        !peers.take_revoked(&owner) && !peers.take_revoked("guest-5150"),
        "signing out is not a revocation: no revoked mark"
    );
}

/// A device row deleted directly (no queue entry) closes that owner peer on
/// the next five-second sweep — and never a guest peer, which has no row.
#[tokio::test]
async fn removed_device_row_closes_the_owner_peer_but_never_a_guest() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    let owner = pair_owned(&db, "wiped laptop");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let owner_token = live_peer(&peers, &owner);
    let guest_token = live_peer(&peers, "guest-9999");
    // Direct database change: no queue entry, exactly the case the tick
    // exists for.
    assert!(
        RemoteDevices::new(db.clone())
            .revoke(&owner)
            .expect("revoke"),
        "the device row is removed"
    );
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    wait_cancelled(&owner_token, Duration::from_secs(6), "a removed device row").await;
    reaper.abort();
    assert!(
        peers.take_revoked(&owner),
        "a removed device closes revoked, like a revocation"
    );
    assert!(
        !guest_token.is_cancelled(),
        "a guest peer has no device row and must survive the sweep"
    );
    assert!(
        !peers.take_revoked("guest-9999"),
        "the surviving guest hears no revoked frame"
    );
}

/// The synchronous unit: draining the queues closes the named live peer.
#[test]
fn drain_queues_closes_the_named_live_peer_at_once() {
    let (_dir, db) = scratch();
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let token = live_peer(&peers, "drain-device");
    queue_revocation(&db, "drain-device");
    assert_eq!(drain_queues(&db, &peers, &connections), 1);
    assert!(token.is_cancelled());
    assert!(peers.take_revoked("drain-device"));
}

/// `close_all` drops every live peer with no farewell mark.
#[test]
fn close_all_drops_owner_and_guest_peers_silently() {
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let owner = live_peer(&peers, "owner-device");
    let guest = live_peer(&peers, "guest-77");
    assert_eq!(close_all(&peers, &connections), 2);
    assert!(owner.is_cancelled() && guest.is_cancelled());
    assert!(peers.live_ids().is_empty());
    assert!(!peers.take_revoked("owner-device"));
}

/// Minimal handler deps, mirroring the admin route's own test harness.
fn test_deps(db: &Arc<Database>, scratch: &std::path::Path) -> HandlerDeps {
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(crate::executor::Executor::new(
        Arc::clone(db),
        Arc::clone(&notif),
    ));
    let graph_engine = Arc::new(crate::graph_engine::GraphEngine::new(
        Arc::clone(db),
        Arc::clone(&notif),
    ));
    let watcher_engine = Arc::new(crate::watchers::WatcherEngine::new(
        Arc::clone(db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(crate::rag::ingestion::IngestionManager::new(
        Arc::clone(db),
        scratch.to_path_buf(),
    ));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
        scratch.join("skills"),
        Vec::new(),
        15,
    ));
    HandlerDeps {
        db: Arc::clone(db),
        executor,
        watcher_engine,
        scheduler_notify: Arc::new(tokio::sync::Notify::new()),
        scheduled_prompt_notify: Arc::new(tokio::sync::Notify::new()),
        graph_engine,
        notification_service: notif,
        ingestion,
        dynamic_skills,
        shelf: crate::sessions::shelf::ShelfStore::for_test(db),
        start_time: std::time::Instant::now(),
        port: 0,
    }
}

/// Unwrap one handler reply to its JSON body.
fn reply_json(result: &rmcp::model::CallToolResult) -> serde_json::Value {
    let text = result.content[0]
        .as_text()
        .expect("the reply carries text")
        .text
        .clone();
    serde_json::from_str(&text).expect("the reply is JSON")
}

/// The admin `revoke` reply carries `closed: 0` when nothing was live; the
/// `closed: 1` live case runs inside the admin route's own serialized test.
#[tokio::test]
async fn admin_revoke_reply_carries_closed_zero_when_nothing_is_live() {
    let scratch = tempfile::tempdir().expect("scratch");
    let db = Arc::new(Database::new(&scratch.path().join("test.db")).expect("database"));
    let handler = TaskTriggerHandler::with_deps(test_deps(&db, scratch.path()));
    let id = pair_owned(&db, "idle laptop");
    assert!(
        !crate::remote::live::shared()
            .live_devices()
            .iter()
            .any(|live| live == &id),
        "no other test may hold this device live"
    );
    let reply = handler.remote_revoke_impl(&id).expect("revoke");
    assert_eq!(reply_json(&reply), json!({ "revoked": id, "closed": 0 }));
}

/// A revoked device cannot re-attach: with no device row its offer is
/// refused as unknown before any peer is created.
#[test]
fn revoked_device_offer_is_refused_as_unknown_device() {
    let (dir, db) = scratch();
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let devices = RemoteDevices::new(db.clone());
    let id = pair_owned(&db, "revoked laptop");
    let now = chrono::Utc::now().timestamp();
    let unknown = json!({"t": "offer", "device_id": id});
    assert_eq!(
        verify_offer(&devices, &host.host_id(), &unknown, now).err(),
        Some(Refusal::BadSignature),
        "with the row present the offer gets past the device check"
    );
    assert!(
        devices.revoke(&id).expect("revoke"),
        "the device row is removed"
    );
    assert_eq!(
        verify_offer(&devices, &host.host_id(), &unknown, now).err(),
        Some(Refusal::UnknownDevice),
        "a revoked device's offer is refused: it cannot re-attach"
    );
}
