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
use crate::remote::connection::{DevicePeers, GuestMeta};
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::HostKey;
use crate::remote::relay::{queue_revocation, queue_trust_revocation, ConnectionRegistry};
use crate::remote::signaling::{verify_offer, Refusal};
use crate::remote::trust::Grant;
use std::collections::HashSet;

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
        hub: None,
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

/// Register a live guest peer for `key` with the grant snapshot it opened
/// with, and return its token, cancelled on close.
fn live_guest(
    peers: &DevicePeers,
    key: &str,
    login: &str,
    grant: Grant,
    upload: bool,
) -> Arc<CancellationToken> {
    let token = Arc::new(CancellationToken::new());
    let (control, _inbox) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        peers.register_guest(
            key,
            &format!("d:{key}"),
            Arc::clone(&token),
            control,
            GuestMeta {
                login: crate::remote::trust::normalize_login(login),
                grant,
                upload,
            },
        ),
        "a fresh guest peer registers"
    );
    token
}

/// Store a scoped trust row for `login`.
fn trust_scoped(db: &Database, login: &str, sessions: &[&str]) {
    crate::remote::panel::trust_add(
        db,
        login,
        false,
        &sessions.iter().map(ToString::to_string).collect::<Vec<_>>(),
        false,
    )
    .expect("trust row");
}

fn sessions_grant(names: &[&str]) -> Grant {
    Grant::Sessions(
        names
            .iter()
            .map(ToString::to_string)
            .collect::<HashSet<_>>(),
    )
}

/// `Grant::covers`: `All` covers anything, `Sessions` covers only its
/// subsets, and never `All`.
#[test]
fn grant_covers_truth_table() {
    let all = Grant::All;
    let pair = sessions_grant(&["a", "b"]);
    assert!(all.covers(&all));
    assert!(all.covers(&pair));
    assert!(all.covers(&sessions_grant(&[])));
    assert!(pair.covers(&pair));
    assert!(pair.covers(&sessions_grant(&["a"])));
    assert!(pair.covers(&sessions_grant(&[])));
    assert!(!pair.covers(&all));
    assert!(!pair.covers(&sessions_grant(&["a", "c"])));
    assert!(!sessions_grant(&[]).covers(&sessions_grant(&["a"])));
    assert!(sessions_grant(&[]).covers(&sessions_grant(&[])));
}

/// `trust_remove` drains through the queue and closes the login's guest
/// peer at once with a revoked farewell, while an owner peer stays open.
#[test]
fn trust_remove_queue_closes_the_guest_peer_with_a_revoked_farewell() {
    let (_dir, db) = scratch();
    trust_scoped(&db, "bob", &["a"]);
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let guest = live_guest(&peers, "guest-101", "bob", sessions_grant(&["a"]), false);
    let owner_token = live_peer(&peers, "owner-device");
    assert!(crate::remote::panel::trust_remove(&db, "bob").expect("removed"));
    assert_eq!(drain_queues(&db, &peers, &connections), 1);
    assert!(guest.is_cancelled());
    assert!(peers.take_revoked("guest-101"));
    assert!(
        !owner_token.is_cancelled(),
        "an owner peer is never matched by a trust revocation"
    );
}

/// A queued trust revocation lands on the next one-second tick: the guest
/// hears `revoked` and its channel closes within the tick.
#[tokio::test]
async fn trust_remove_closes_a_registered_guest_within_the_tick() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    trust_scoped(&db, "bob", &["a"]);
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let guest = live_guest(&peers, "guest-202", "bob", sessions_grant(&["a"]), false);
    let owner_token = live_peer(&peers, "owner-device");
    queue_trust_revocation(&db, "bob");
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    wait_cancelled(&guest, Duration::from_millis(1500), "a trust removal").await;
    reaper.abort();
    assert!(
        peers.take_revoked("guest-202"),
        "the guest hears the revoked frame first"
    );
    assert!(
        !owner_token.is_cancelled(),
        "an owner peer survives a trust removal"
    );
}

/// A narrowing `trust_add` (all to scoped) queues the login, and the next
/// tick closes the guest; a widened grant queues nothing and stays open.
#[tokio::test]
async fn narrowing_trust_add_closes_the_guest_widening_keeps_it() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    crate::remote::panel::trust_add(&db, "narrow", true, &[], false).expect("full grant");
    crate::remote::panel::trust_add(&db, "wide", false, &["a".to_string()], false)
        .expect("scoped grant");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let narrowed = live_guest(&peers, "guest-301", "narrow", Grant::All, false);
    let widened = live_guest(&peers, "guest-302", "wide", sessions_grant(&["a"]), false);
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    crate::remote::panel::trust_add(&db, "narrow", false, &["a".to_string()], false)
        .expect("narrowed");
    crate::remote::panel::trust_add(&db, "wide", true, &[], false).expect("widened");
    wait_cancelled(&narrowed, Duration::from_secs(2), "a narrowed grant").await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    reaper.abort();
    assert!(peers.take_revoked("guest-301"));
    assert!(!widened.is_cancelled(), "a widened grant closes nothing");
}

/// Withdrawing `upload` narrows the grant: the queued login closes the
/// guest on the next tick even though its sessions did not change.
#[test]
fn upload_withdrawn_closes_the_guest_peer() {
    let (_dir, db) = scratch();
    crate::remote::panel::trust_add(&db, "camper", false, &["a".to_string()], true)
        .expect("upload granted");
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let guest = live_guest(&peers, "guest-401", "camper", sessions_grant(&["a"]), true);
    crate::remote::panel::trust_add(&db, "camper", false, &["a".to_string()], false)
        .expect("upload withdrawn");
    assert_eq!(drain_queues(&db, &peers, &connections), 1);
    assert!(guest.is_cancelled());
    assert!(peers.take_revoked("guest-401"));
}

/// A trust row deleted directly (no queue entry) closes the guest on the
/// next five-second sweep — the case the recheck exists for.
#[tokio::test]
async fn direct_trust_delete_closes_the_guest_within_five_seconds() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    trust_scoped(&db, "ghost", &["a"]);
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let guest = live_guest(&peers, "guest-501", "ghost", sessions_grant(&["a"]), false);
    let owner = pair_owned(&db, "steady laptop");
    let owner_token = live_peer(&peers, &owner);
    db.remove_trust("ghost").expect("direct row delete");
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    wait_cancelled(
        &guest,
        Duration::from_secs(6),
        "a directly deleted trust row",
    )
    .await;
    reaper.abort();
    assert!(peers.take_revoked("guest-501"));
    assert!(
        !owner_token.is_cancelled(),
        "an owner peer survives the trust sweep"
    );
}

/// A guest whose grant is unchanged stays open across several recheck
/// passes, and so does an owner peer beside it.
#[tokio::test]
async fn unchanged_grant_survives_rechecks_and_owner_stays_open() {
    let (dir, db) = scratch();
    test_config(&dir, true);
    test_identity(&dir);
    trust_scoped(&db, "steady", &["a"]);
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let guest = live_guest(&peers, "guest-601", "steady", sessions_grant(&["a"]), false);
    let owner = pair_owned(&db, "steady laptop");
    let owner_token = live_peer(&peers, &owner);
    let reaper = tokio::spawn(run(
        dir.path().to_path_buf(),
        db.clone(),
        peers.clone(),
        connections,
    ));
    tokio::time::sleep(Duration::from_secs(6)).await;
    reaper.abort();
    assert!(
        !guest.is_cancelled(),
        "an unchanged grant stays open across rechecks"
    );
    assert!(
        !owner_token.is_cancelled(),
        "an owner peer stays open across rechecks"
    );
}

/// The MCP `trust_remove` reply carries how many live guest peers closed:
/// one for a live guest, zero when nothing is live.
#[tokio::test]
async fn mcp_trust_remove_reply_carries_closed_count() {
    let scratch = tempfile::tempdir().expect("scratch");
    let db = Arc::new(Database::new(&scratch.path().join("test.db")).expect("database"));
    let handler = TaskTriggerHandler::with_deps(test_deps(&db, scratch.path()));
    crate::remote::panel::trust_add(&db, "mcpbob", false, &["a".to_string()], false)
        .expect("trust row");
    let peers = crate::remote::live::shared().peers().clone();
    let token = Arc::new(CancellationToken::new());
    let (control, _inbox) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        peers.register_guest(
            "guest-mcp-1",
            "d:guest-mcp-1",
            Arc::clone(&token),
            control,
            GuestMeta {
                login: "mcpbob".to_string(),
                grant: sessions_grant(&["a"]),
                upload: false,
            },
        ),
        "the live guest registers on the shared peers"
    );
    let reply = handler
        .remote_trust_remove_impl("mcpbob")
        .expect("trust_remove");
    assert_eq!(
        reply_json(&reply),
        json!({ "removed": "mcpbob", "closed": 1 })
    );
    assert!(token.is_cancelled());
    peers.close("guest-mcp-1");
    assert!(peers.take_revoked("guest-mcp-1"));

    crate::remote::panel::trust_add(&db, "mcpbob", false, &["a".to_string()], false)
        .expect("trust row again");
    let reply = handler
        .remote_trust_remove_impl("mcpbob")
        .expect("trust_remove");
    assert_eq!(
        reply_json(&reply),
        json!({ "removed": "mcpbob", "closed": 0 })
    );
}
