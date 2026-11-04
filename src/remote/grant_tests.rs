//! The grant matrix: who sees which sessions, and what revoking does.
//!
//! Untrusted logins resolve to nothing and offers from them are refused;
//! the owner resolves to [`Grant::All`] with no trust row; a full grant
//! sees every agent session; a partial grant lists only its subset and
//! meets `not_found` everywhere else; removing or narrowing a profile
//! kicks live guest peers with a farewell. Each test breaks when its
//! production check is skipped: the pairing bind, the bridge filter, or
//! the kick.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};

use super::{
    allowlist_fingerprint, guest_allow_body, normalize_login, resolve, resolve_upload, Grant,
};
use crate::application::ports::StateRepository as _;
use crate::db::Database;
use crate::remote::bridge::RemoteBridge;
use crate::remote::devices::{device_id, RemoteDevices};
use crate::remote::guest_allow;
use crate::remote::hostkey::HostKey;
use crate::remote::invite::PublishedInvite;
use crate::remote::pairing::{derive_k_pair, mac_init, normalize_code, PairingState};
use crate::remote::panel;
use crate::remote::relay::{ConnectionRegistry, RelayRuntime, REMOTE_TRUST_REVOKE_KEY};
use crate::remote::signaling::{offer_message, verify_offer, Refusal};
use sha2::{Digest as _, Sha256};

use crate::sessions::protocol::{
    attach_frame, list_frame, resize_frame, shelf_delete_frame, shelf_get_frame, shelf_list_frame,
    shelf_put_begin_frame, shelf_put_end_frame, shelf_put_part_frame, ERR_BUSY, ERR_FORBIDDEN,
    ERR_NOT_FOUND,
};
use crate::sessions::ws::SessionsState;
use crate::sessions::{HubEvent, HubSession, SessionHub, SessionKind, SpawnRequest};

const OWNER: &str = "octocat";
const GUEST: &str = "guest";

fn test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    (dir, db)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[test]
fn untrusted_login_resolves_to_none_and_refuses_with_forbidden() {
    let (_dir, db) = test_db();
    assert_eq!(resolve(&db, OWNER, "mallory"), None);
    assert_eq!(resolve(&db, OWNER, "  "), None);
    assert_eq!(Refusal::Forbidden.reason(), "forbidden");
    let fail = crate::remote::signaling::connect_fail(Refusal::Forbidden);
    assert_eq!(fail["t"], "connect_fail");
    assert_eq!(fail["reason"], "forbidden");
}

#[test]
fn owner_resolves_to_all_with_no_trust_row() {
    let (_dir, db) = test_db();
    assert_eq!(resolve(&db, OWNER, OWNER), Some(Grant::All));
    assert_eq!(resolve(&db, OWNER, "OctoCat"), Some(Grant::All));
    assert_eq!(resolve(&db, "  octocat ", "OCTOCAT"), Some(Grant::All));
    assert_eq!(normalize_login("  OctoCat "), "octocat");
}

#[test]
fn trust_store_round_trips_profiles_and_grants() {
    let (_dir, db) = test_db();
    assert_eq!(db.get_trust(GUEST).expect("get"), None);

    db.upsert_trust(" Guest ", false).expect("upsert");
    db.set_grants(
        GUEST,
        &["beta".to_string(), " alpha ".to_string(), String::new()],
    )
    .expect("grants");
    let profile = db.get_trust("GUEST").expect("get").expect("present");
    assert_eq!(profile.login, GUEST);
    assert!(!profile.all_sessions);
    assert_eq!(profile.sessions, vec!["alpha", "beta"]);

    db.upsert_trust(GUEST, true).expect("widen");
    db.set_grants(GUEST, &[]).expect("clear");
    let widened = db.get_trust(GUEST).expect("get").expect("present");
    assert!(widened.all_sessions);
    assert!(widened.sessions.is_empty());

    let listed = db.list_trust().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].login, GUEST);

    assert!(db.remove_trust(GUEST).expect("remove"));
    assert!(!db.remove_trust(GUEST).expect("remove again"));
    assert_eq!(db.get_trust(GUEST).expect("get"), None);
    assert!(db.list_trust().expect("list").is_empty());
}

#[test]
fn scoped_resolve_collects_the_stored_names() {
    let (_dir, db) = test_db();
    db.upsert_trust(GUEST, false).expect("upsert");
    db.set_grants(GUEST, &["Alpha".to_string()])
        .expect("grants");
    let grant = resolve(&db, OWNER, GUEST).expect("a grant");
    assert!(grant.allows("Alpha"));
    assert!(!grant.allows("alpha"), "session names keep their case");
    assert!(!grant.allows("beta"));
}

/// A spawned session, killed when the guard drops so no test leaves a child.
struct KillGuard {
    hub: SessionHub,
    id: String,
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        let _ = self.hub.kill(&self.id);
    }
}

/// Spawn a `cat` session with a fixed name; `cat` echoes its PTY input.
fn spawn_named(
    hub: &SessionHub,
    name: &str,
    kind: SessionKind,
    workdir: &Path,
) -> (Arc<HubSession>, KillGuard) {
    let request = SpawnRequest {
        kind,
        cli: "cat".to_string(),
        model: None,
        workdir: workdir.to_string_lossy().to_string(),
        name: Some(name.to_string()),
        prompt: None,
        interactive_args: None,
        fallback_args: None,
        model_flag: None,
        id: None,
        harness_conversation_id: None,
        seed_id: None,
        accent: None,
        yolo: false,
        preserve_args: false,
    };
    let session = hub.spawn(&request).expect("test spawn");
    assert_eq!(session.name, name);
    let guard = KillGuard {
        hub: hub.clone(),
        id: session.id.clone(),
    };
    (session, guard)
}

/// A hub with two named agent sessions plus one terminal, and the guards
/// that keep them alive.
struct GrantHub {
    state: Arc<SessionsState>,
    alpha: Arc<HubSession>,
    beta: Arc<HubSession>,
    shell: Arc<HubSession>,
    _kills: Vec<KillGuard>,
    _dir: tempfile::TempDir,
}

fn grant_hub() -> GrantHub {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Database::new(&dir.path().join("test.db")).expect("test database"));
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
    let workdir = tempfile::tempdir().expect("workdir");
    let (alpha, kill_alpha) = spawn_named(&hub, "alpha", SessionKind::Agent, workdir.path());
    let (beta, kill_beta) = spawn_named(&hub, "beta", SessionKind::Agent, workdir.path());
    let (shell, kill_terminal) = spawn_named(&hub, "shell", SessionKind::Terminal, workdir.path());
    let state = SessionsState::for_test(hub, "token".to_string());
    GrantHub {
        state,
        alpha,
        beta,
        shell,
        _kills: vec![kill_alpha, kill_beta, kill_terminal],
        _dir: dir,
    }
}

async fn recv_frame(rx: &mut crate::sessions::outbound::ConnRx) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a frame arrives")
        .expect("channel open");
    serde_json::from_str(&frame).expect("frames are JSON")
}

fn session_names(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("a sessions array")
        .iter()
        .map(|item| item["name"].as_str().expect("a name").to_string())
        .collect()
}

#[tokio::test]
async fn full_grant_lists_every_agent_session_and_attaches() {
    let hub = grant_hub();
    let (_dir, db) = test_db();
    db.upsert_trust(GUEST, true).expect("trust");
    let grant = resolve(&db, OWNER, GUEST).expect("a grant");
    assert_eq!(grant, Grant::All);

    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);
    bridge.handle_text(&list_frame("r1"), &out_tx).await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(list["t"], "sessions");
    let names = session_names(&list);
    assert!(names.contains(&"alpha".to_string()), "{names:?}");
    assert!(names.contains(&"beta".to_string()), "{names:?}");
    assert!(
        names.contains(&"shell".to_string()),
        "terminals list for a full grant: {names:?}"
    );

    bridge
        .handle_text(&attach_frame(&hub.beta.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["id"], hub.beta.id);
    // Drain the snapshot's continuations before the next attach.
    loop {
        let more = recv_frame(&mut out_rx).await;
        assert_eq!(more["t"], "snapshot_more", "got {more}");
        if more["last"].as_bool().expect("the last flag") {
            break;
        }
    }

    // The terminal attaches too.
    bridge
        .handle_text(&attach_frame(&hub.shell.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["id"], hub.shell.id);
    bridge.abort();
}

#[tokio::test]
async fn partial_grant_lists_only_the_subset_and_hides_the_rest() {
    let hub = grant_hub();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);

    bridge.handle_text(&list_frame("r1"), &out_tx).await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(session_names(&list), vec!["alpha".to_string()]);

    // Every interaction outside the grant answers `not_found`, never
    // `forbidden`: the guest must not learn the session exists.
    bridge
        .handle_text(&attach_frame(&hub.beta.id), &out_tx)
        .await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["t"], "error");
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let prompt = json!({"t": "prompt", "req": "r2", "id": hub.beta.id, "text": "hi"}).to_string();
    bridge.handle_text(&prompt, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let kill = json!({"t": "kill", "req": "r3", "id": hub.beta.id}).to_string();
    bridge.handle_text(&kill, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let input = json!({"t": "input", "id": hub.beta.id, "data": b64(b"x")}).to_string();
    bridge.handle_text(&input, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    // The ungranted terminal meets the same `not_found` on every touch.
    bridge
        .handle_text(&attach_frame(&hub.shell.id), &out_tx)
        .await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let prompt = json!({"t": "prompt", "req": "r4", "id": hub.shell.id, "text": "hi"}).to_string();
    bridge.handle_text(&prompt, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let kill = json!({"t": "kill", "req": "r5", "id": hub.shell.id}).to_string();
    bridge.handle_text(&kill, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    let input = json!({"t": "input", "id": hub.shell.id, "data": b64(b"x")}).to_string();
    bridge.handle_text(&input, &out_tx).await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND);

    // The granted session still attaches.
    bridge
        .handle_text(&attach_frame(&hub.alpha.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    bridge.abort();

    // Granted the shell by name, the guest lists and attaches it.
    let grant = Grant::Sessions(HashSet::from(["shell".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);
    bridge.handle_text(&list_frame("r6"), &out_tx).await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(session_names(&list), vec!["shell".to_string()]);
    bridge
        .handle_text(&attach_frame(&hub.shell.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["id"], hub.shell.id);
    bridge.abort();
}

#[test]
fn partial_grant_filters_lifecycle_events_to_its_names() {
    let hub = grant_hub();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);

    bridge.handle_event(HubEvent::SessionAdded(Box::new(hub.beta.item())), &out_tx);
    assert!(
        out_rx.try_recv().is_err(),
        "an ungranted arrival must not forward"
    );

    bridge.handle_event(HubEvent::SessionAdded(Box::new(hub.shell.item())), &out_tx);
    assert!(
        out_rx.try_recv().is_err(),
        "an ungranted terminal arrival must not forward"
    );

    bridge.handle_event(HubEvent::SessionAdded(Box::new(hub.alpha.item())), &out_tx);
    let added: Value =
        serde_json::from_str(&out_rx.try_recv().expect("the granted arrival")).expect("JSON");
    assert_eq!(added["t"], "session_added");

    bridge.handle_event(HubEvent::SessionRemoved(hub.beta.id.clone()), &out_tx);
    assert!(
        out_rx.try_recv().is_err(),
        "an ungranted removal must not forward"
    );
    bridge.abort();
}

#[tokio::test]
async fn natural_exit_removal_reaches_holders_not_ungranted() {
    let hub = grant_hub();

    // A full-grant holder that listed the session hears its natural-exit
    // removal; the attached-only `exited` stays silent for a list-holder.
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, false);
    bridge.handle_text(&list_frame("r1"), &out_tx).await;
    assert_eq!(recv_frame(&mut out_rx).await["t"], "sessions");
    bridge.handle_event(
        HubEvent::Exited {
            id: hub.alpha.id.clone(),
            code: 0,
            to_all: false,
        },
        &out_tx,
    );
    bridge.handle_event(HubEvent::SessionRemoved(hub.alpha.id.clone()), &out_tx);
    let removed: Value =
        serde_json::from_str(&out_rx.try_recv().expect("the removal reaches the holder"))
            .expect("JSON");
    assert_eq!(removed["t"], "session_removed");
    assert_eq!(removed["id"], hub.alpha.id);
    assert!(
        out_rx.try_recv().is_err(),
        "only the removal forwards to a list-holder"
    );
    bridge.abort();

    // A guest granted only beta listed its subset: alpha's natural exit
    // leaks nothing through either event.
    let grant = Grant::Sessions(HashSet::from(["beta".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);
    bridge.handle_text(&list_frame("r1"), &out_tx).await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(session_names(&list), vec!["beta".to_string()]);
    bridge.handle_event(
        HubEvent::Exited {
            id: hub.alpha.id.clone(),
            code: 0,
            to_all: false,
        },
        &out_tx,
    );
    bridge.handle_event(HubEvent::SessionRemoved(hub.alpha.id.clone()), &out_tx);
    assert!(
        out_rx.try_recv().is_err(),
        "an ungranted natural exit forwards nothing"
    );
    bridge.abort();
}

/// A hub resize reaches a remote peer as `resized{id,cols,rows}` for the
/// session it is attached to, and never for one it is not attached to
/// (granted or not).
#[tokio::test]
async fn resized_reaches_attached_remote_peers_only() {
    let hub = grant_hub();
    let grant = Grant::Sessions(HashSet::from(["beta".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);
    bridge
        .handle_text(&attach_frame(&hub.beta.id), &out_tx)
        .await;
    assert_eq!(recv_frame(&mut out_rx).await["t"], "snapshot");
    loop {
        let more = recv_frame(&mut out_rx).await;
        if more["last"].as_bool().expect("the last flag") {
            break;
        }
    }

    for id in [&hub.alpha.id, &hub.shell.id] {
        bridge.handle_event(
            HubEvent::Resized {
                id: id.clone(),
                cols: 132,
                rows: 43,
            },
            &out_tx,
        );
    }
    assert!(
        out_rx.try_recv().is_err(),
        "a resize of a session the peer is not attached to forwards nothing"
    );

    bridge.handle_event(
        HubEvent::Resized {
            id: hub.beta.id.clone(),
            cols: 132,
            rows: 43,
        },
        &out_tx,
    );
    assert_eq!(
        recv_frame(&mut out_rx).await,
        json!({"t": "resized", "id": hub.beta.id, "cols": 132, "rows": 43})
    );
    bridge.abort();
}

/// The owner's paired device sizes a session: the size is clamped into
/// bounds, applied to the PTY, and every other attached peer hears
/// `resized`.
#[tokio::test]
async fn owner_device_resize_applies_clamped_and_reaches_other_peers() {
    let hub = grant_hub();
    let mut events = hub.state.hub.subscribe_events();
    let (owner_tx, mut owner_rx) = crate::sessions::outbound::channel();
    let mut owner = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, true);
    owner.set_owner(true);
    let (other_tx, mut other_rx) = crate::sessions::outbound::channel();
    let mut other = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, false);
    other
        .handle_text(&attach_frame(&hub.alpha.id), &other_tx)
        .await;
    assert_eq!(recv_frame(&mut other_rx).await["t"], "snapshot");
    loop {
        if recv_frame(&mut other_rx).await["last"].as_bool() == Some(true) {
            break;
        }
    }

    owner
        .handle_text(&resize_frame(&hub.alpha.id, 5000, 1), &owner_tx)
        .await;
    assert!(owner_rx.try_recv().is_err(), "a resize answers nothing");
    let item = hub.alpha.item();
    assert_eq!(
        (item.cols, item.rows),
        (500, 5),
        "clamped to the upper/lower bounds"
    );
    let event = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(event @ HubEvent::Resized { .. }) = events.recv().await {
                return event;
            }
        }
    })
    .await
    .expect("the hub announces the resize");
    other.handle_event(event, &other_tx);
    assert_eq!(
        recv_frame(&mut other_rx).await,
        json!({"t": "resized", "id": hub.alpha.id, "cols": 500, "rows": 5})
    );

    tokio::time::sleep(Duration::from_millis(150)).await;
    owner
        .handle_text(&resize_frame(&hub.alpha.id, 1, 9999), &owner_tx)
        .await;
    let item = hub.alpha.item();
    assert_eq!((item.cols, item.rows), (20, 200), "clamped the other way");
    owner.abort();
    other.abort();
}

/// Resizes closer than the minimum gap coalesce: the first applies, the
/// rest wait and only the latest lands once the gap has passed.
#[tokio::test]
async fn owner_device_resizes_are_throttled_latest_wins() {
    let hub = grant_hub();
    let (out_tx, _out_rx) = crate::sessions::outbound::channel();
    let mut owner = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, true);
    owner.set_owner(true);
    for (cols, rows) in [(100, 30), (110, 31), (120, 32)] {
        owner
            .handle_text(&resize_frame(&hub.beta.id, cols, rows), &out_tx)
            .await;
    }
    let item = hub.beta.item();
    assert_eq!(
        (item.cols, item.rows),
        (100, 30),
        "only the first applies at once"
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    let item = hub.beta.item();
    assert_eq!(
        (item.cols, item.rows),
        (120, 32),
        "the latest lands after the gap"
    );
    owner.abort();
}

/// Every guest is refused: `forbidden` for a session it can see (narrow or
/// full grant alike), `not_found` outside its grant, and the size is left
/// alone.
#[tokio::test]
async fn guest_resize_is_refused() {
    let hub = grant_hub();
    let before = hub.alpha.item();

    let narrow = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut guest = RemoteBridge::with_grant(Arc::clone(&hub.state), narrow, true);
    guest
        .handle_text(&resize_frame(&hub.alpha.id, 120, 40), &out_tx)
        .await;
    assert_eq!(recv_frame(&mut out_rx).await["code"], ERR_FORBIDDEN);
    guest
        .handle_text(&resize_frame(&hub.beta.id, 120, 40), &out_tx)
        .await;
    assert_eq!(recv_frame(&mut out_rx).await["code"], ERR_NOT_FOUND);
    guest.abort();

    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut full_guest = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, true);
    full_guest
        .handle_text(&resize_frame(&hub.alpha.id, 120, 40), &out_tx)
        .await;
    assert_eq!(recv_frame(&mut out_rx).await["code"], ERR_FORBIDDEN);
    full_guest.abort();

    let after = hub.alpha.item();
    assert_eq!((after.cols, after.rows), (before.cols, before.rows));
}

#[test]
fn owner_login_matches_trimmed_and_case_insensitively() {
    assert!(super::is_owner_login(OWNER, " OctoCat "));
    assert!(!super::is_owner_login(OWNER, GUEST));
    assert!(!super::is_owner_login(OWNER, "  "));
}

#[test]
fn pairing_binds_the_claimed_login_and_refuses_untrusted_guests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_db_dir, db) = test_db();
    let devices = RemoteDevices::new(db.clone());
    let state = PairingState::new(dir.path());
    let host = HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key");
    let device = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    let created = state.create(&host.host_id(), 1_000).expect("a code");
    let key = derive_k_pair(&normalize_code(&created.code), &host.host_id());
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let body = json!({
        "t": "pair_init",
        "device_spki": device.spki_b64(),
        "device_label": "guest laptop",
        "nonce_b": nonce_b,
        "mac": b64(&mac_init(&key, &device.spki_b64(), &nonce_b)),
        "github_login": GUEST,
    });

    // Untrusted: refused exactly like a wrong code, and the device is not
    // stored — but the code survives for the trusted retry below.
    let refused = state
        .respond_with_nonce(&body, &host, &devices, OWNER, 1_001, [0u8; 16])
        .expect("respond");
    assert_eq!(refused["t"], "pair_fail");
    assert_eq!(refused["reason"], "bad_code");
    assert!(devices.list().expect("list").is_empty());

    db.upsert_trust(GUEST, true).expect("trust");
    let paired = state
        .respond_with_nonce(&body, &host, &devices, OWNER, 1_002, [0u8; 16])
        .expect("respond");
    assert_eq!(paired["t"], "pair_ok");
    let stored = devices.list().expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].github_login, GUEST);
    assert_eq!(stored[0].id, device_id(device.spki_der()));
}

#[test]
fn offer_with_a_mismatched_login_is_refused() {
    let (_dir, db) = test_db();
    let devices = RemoteDevices::new(db);
    let device = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    devices
        .pair(
            device.spki_der().to_vec(),
            "laptop",
            GUEST,
            "2026-01-01T00:00:00Z",
        )
        .expect("pair");
    let host_id = "76805a63a6980c18";
    let ts = chrono::Utc::now().timestamp();
    let sdp = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\n";
    let sig = device.sign(offer_message(host_id, ts, sdp).as_bytes());
    let mut body = json!({
        "t": "offer",
        "device_id": device_id(device.spki_der()),
        "ts": ts,
        "sdp": sdp,
        "sig": b64(&sig),
        "github_login": "mallory",
    });
    assert_eq!(
        verify_offer(&devices, host_id, &body, ts).err(),
        Some(Refusal::BadSignature),
        "a login that is not the bound one refuses the offer"
    );

    body["github_login"] = json!(GUEST);
    let offer = verify_offer(&devices, host_id, &body, ts).expect("the bound login verifies");
    assert_eq!(offer.device.github_login, GUEST);

    body.as_object_mut().expect("object").remove("github_login");
    verify_offer(&devices, host_id, &body, ts).expect("legacy offers without a login verify");
}

#[test]
fn removing_a_profile_kicks_its_live_guests_with_a_farewell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_db_dir, db) = test_db();
    let devices = RemoteDevices::new(db.clone());
    let guest_id = devices
        .pair(
            b"guest spki bytes".to_vec(),
            "guest laptop",
            GUEST,
            "2026-01-01T00:00:00Z",
        )
        .expect("pair")
        .id;
    let owner_id = devices
        .pair(
            b"owner spki bytes".to_vec(),
            "owner laptop",
            OWNER,
            "2026-01-01T00:00:00Z",
        )
        .expect("pair")
        .id;
    db.upsert_trust(GUEST, true).expect("trust");

    let host = HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key");
    let runtime = RelayRuntime::new(
        host,
        db.clone(),
        OWNER.to_string(),
        PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    let guest_token = runtime.connections().register(&guest_id);
    let (guest_tx, _guest_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(runtime
        .peers_handle()
        .register(&guest_id, "d:guest", guest_token.clone(), guest_tx));
    let owner_token = runtime.connections().register(&owner_id);
    let (owner_tx, _owner_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(runtime
        .peers_handle()
        .register(&owner_id, "d:owner", owner_token.clone(), owner_tx));

    assert!(panel::trust_remove(&db, GUEST).expect("remove"));
    runtime.drain_remote_revocations();

    assert!(guest_token.is_cancelled(), "the guest peer closes");
    assert!(
        runtime.peers_handle().take_revoked(&guest_id),
        "the closed guest hears the farewell first"
    );
    assert!(
        !owner_token.is_cancelled(),
        "another login's peer survives the kick"
    );
    let queue = db
        .get_state(REMOTE_TRUST_REVOKE_KEY)
        .expect("queue read")
        .unwrap_or_default();
    assert!(queue.is_empty(), "the drain consumes the queue: {queue}");
    assert!(
        !panel::trust_remove(&db, GUEST).expect("remove again"),
        "an unknown login removes nothing"
    );
}

#[test]
fn narrowing_a_grant_kicks_but_widening_does_not() {
    let (_dir, db) = test_db();
    db.upsert_trust(GUEST, true).expect("trust");
    db.set_grants(GUEST, &[]).expect("grants");

    // All-to-scoped narrows: the login is queued for a kick.
    panel::trust_add(&db, GUEST, false, &["alpha".to_string()], false).expect("narrow");
    let queue = db
        .get_state(REMOTE_TRUST_REVOKE_KEY)
        .expect("queue read")
        .unwrap_or_default();
    assert!(queue.contains(GUEST), "a narrowed grant kicks: {queue}");

    // Scoped-to-all widens: nothing is queued.
    db.set_state(REMOTE_TRUST_REVOKE_KEY, "").expect("clear");
    panel::trust_add(&db, GUEST, true, &[], false).expect("widen");
    let queue = db
        .get_state(REMOTE_TRUST_REVOKE_KEY)
        .expect("queue read")
        .unwrap_or_default();
    assert!(queue.is_empty(), "a widened grant kicks nothing: {queue}");

    // Dropping a name narrows; keeping the set does not.
    panel::trust_add(
        &db,
        GUEST,
        false,
        &["alpha".to_string(), "beta".to_string()],
        false,
    )
    .expect("scoped");
    db.set_state(REMOTE_TRUST_REVOKE_KEY, "").expect("clear");
    panel::trust_add(&db, GUEST, false, &["alpha".to_string()], false).expect("drop");
    let queue = db
        .get_state(REMOTE_TRUST_REVOKE_KEY)
        .expect("queue read")
        .unwrap_or_default();
    assert!(queue.contains(GUEST), "dropping a name kicks: {queue}");
}

/// A file that sniffs as PNG: the 8-byte magic plus a tail (the sniffer
/// only reads the head).
fn png_bytes() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(b"grant-matrix-bytes");
    bytes
}

/// File one PNG onto a session's shelf, or the host shelf for `None`.
fn shelve(state: &SessionsState, name: &str, session_id: Option<&str>) -> String {
    state
        .shelf
        .insert_bytes(&png_bytes(), name.to_string(), session_id, None, "test")
        .expect("shelf insert")
        .file_id
}

fn shelf_names(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("a shelf array")
        .iter()
        .map(|item| item["name"].as_str().expect("a name").to_string())
        .collect()
}

#[tokio::test]
async fn scoped_grant_lists_only_its_shelf_files_and_hides_the_rest() {
    let hub = grant_hub();
    let alpha_file = shelve(&hub.state, "alpha.png", Some(&hub.alpha.id));
    let beta_file = shelve(&hub.state, "beta.png", Some(&hub.beta.id));
    let shell_file = shelve(&hub.state, "shell.png", Some(&hub.shell.id));
    let host_file = shelve(&hub.state, "host.png", None);
    let orphan_file = shelve(&hub.state, "orphan.png", Some("no-such-session"));

    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);

    // The unscoped list is the host shelf: the owner's, never a session
    // guest's, so it lists empty.
    bridge
        .handle_text(&shelf_list_frame("s1", None), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(list["t"], "shelf");
    assert!(shelf_names(&list).is_empty(), "{list}");

    // A scoped list outside the grant is empty, inside it holds the file.
    bridge
        .handle_text(&shelf_list_frame("s2", Some(hub.beta.id.as_str())), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert!(shelf_names(&list).is_empty(), "{list}");

    bridge
        .handle_text(
            &shelf_list_frame("s3", Some(hub.alpha.id.as_str())),
            &out_tx,
        )
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(shelf_names(&list), vec!["alpha.png".to_string()]);

    // Gets and deletes outside the grant answer `not_found` — including a
    // terminal file, which must not leak through `forbidden`, and a file
    // whose session is gone, which fails closed.
    // whose session is gone, which fails closed, and a host-shelf file.
    for file_id in [&beta_file, &shell_file, &orphan_file, &host_file] {
        bridge
            .handle_text(&shelf_get_frame("s4", file_id), &out_tx)
            .await;
        let denied = recv_frame(&mut out_rx).await;
        assert_eq!(denied["t"], "error");
        assert_eq!(denied["code"], ERR_NOT_FOUND, "{denied}");
    }
    for file_id in [&beta_file, &host_file] {
        bridge
            .handle_text(&shelf_delete_frame("s5", file_id), &out_tx)
            .await;
        let denied = recv_frame(&mut out_rx).await;
        assert_eq!(denied["code"], ERR_NOT_FOUND, "{denied}");
    }
    assert!(
        hub.state.shelf.get(&host_file).is_some(),
        "a refused delete leaves the host file in place"
    );

    // The granted file still serves.
    bridge
        .handle_text(&shelf_get_frame("s6", &alpha_file), &out_tx)
        .await;
    let file = recv_frame(&mut out_rx).await;
    assert_eq!(file["t"], "file", "{file}");
    bridge.abort();
}

/// Host-shelf arrivals and removals never reach a session-scoped guest —
/// including the removal of a host file that predates its connection —
/// while a full grant hears both.
#[tokio::test]
async fn host_shelf_pushes_reach_full_grants_only() {
    let hub = grant_hub();
    let old_host = shelve(&hub.state, "old-host.png", None);
    let alpha_file = shelve(&hub.state, "alpha.png", Some(&hub.alpha.id));
    let scoped = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (scoped_tx, mut scoped_rx) = crate::sessions::outbound::channel();
    let mut scoped_bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), scoped, false);
    let (full_tx, mut full_rx) = crate::sessions::outbound::channel();
    let mut full_bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, false);

    let host_item = hub
        .state
        .shelf
        .list(None)
        .into_iter()
        .find(|item| item.file_id == old_host)
        .expect("the host file is listed");
    let alpha_item = hub
        .state
        .shelf
        .list(Some(&hub.alpha.id))
        .into_iter()
        .find(|item| item.file_id == alpha_file)
        .expect("the alpha file is listed");
    // The removal comes first: only the connect-time seeding hides it.
    for event in [
        HubEvent::ShelfRemoved {
            file_id: old_host.clone(),
        },
        HubEvent::ShelfAdded(Box::new(host_item)),
    ] {
        scoped_bridge.handle_event(event.clone(), &scoped_tx);
        full_bridge.handle_event(event, &full_tx);
    }
    assert!(
        scoped_rx.try_recv().is_err(),
        "a session-scoped guest hears nothing about the host shelf"
    );
    assert_eq!(recv_frame(&mut full_rx).await["t"], "shelf_removed");
    assert_eq!(recv_frame(&mut full_rx).await["t"], "shelf_added");

    // A granted session's arrival still reaches the scoped guest.
    scoped_bridge.handle_event(HubEvent::ShelfAdded(Box::new(alpha_item)), &scoped_tx);
    let added = recv_frame(&mut scoped_rx).await;
    assert_eq!(added["t"], "shelf_added");
    assert_eq!(added["item"]["file_id"], alpha_file);
    scoped_bridge.abort();
    full_bridge.abort();
}

#[tokio::test]
async fn full_grant_serves_every_shelf_and_keeps_the_terminal_rule() {
    let hub = grant_hub();
    let beta_file = shelve(&hub.state, "beta.png", Some(&hub.beta.id));
    let shell_file = shelve(&hub.state, "shell.png", Some(&hub.shell.id));
    let orphan_file = shelve(&hub.state, "orphan.png", Some("no-such-session"));
    let host_file = shelve(&hub.state, "host.png", None);

    let (out_tx, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All, false);

    // A scoped list reaches its session's files.
    bridge
        .handle_text(&shelf_list_frame("s1", Some(hub.beta.id.as_str())), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(shelf_names(&list), vec!["beta.png".to_string()]);

    // The host shelf lists and serves for a full grant.
    bridge
        .handle_text(&shelf_list_frame("s1h", None), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(shelf_names(&list), vec!["host.png".to_string()]);
    bridge
        .handle_text(&shelf_get_frame("s1g", &host_file), &out_tx)
        .await;
    let file = recv_frame(&mut out_rx).await;
    assert_eq!(file["t"], "file", "{file}");

    // A file whose session is gone still serves for a full grant — the
    // owner is unchanged by grants.
    bridge
        .handle_text(&shelf_get_frame("s2", &orphan_file), &out_tx)
        .await;
    let file = recv_frame(&mut out_rx).await;
    assert_eq!(file["t"], "file", "{file}");

    // ...while a live terminal file keeps its `forbidden`.
    bridge
        .handle_text(&shelf_get_frame("s3", &shell_file), &out_tx)
        .await;
    let refused = recv_frame(&mut out_rx).await;
    assert_eq!(refused["code"], ERR_FORBIDDEN, "{refused}");

    bridge
        .handle_text(&shelf_delete_frame("s4", &beta_file), &out_tx)
        .await;
    let deleted = recv_frame(&mut out_rx).await;
    assert_eq!(deleted["t"], "ok", "{deleted}");
    bridge.abort();
}

/// A relay runtime over `db` for the allowlist tests: no sessions, no peers —
/// publishing only needs the trust store and the uid cache. Returns the
/// scratch dir too, so it stays alive with the runtime.
fn allowlist_runtime(db: &Database) -> (tempfile::TempDir, RelayRuntime) {
    let dir = tempfile::tempdir().expect("tempdir");
    let host = HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key");
    let runtime = RelayRuntime::new(
        host,
        db.clone(),
        OWNER.to_string(),
        PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    (dir, runtime)
}

#[test]
fn guest_allow_body_is_byte_exact_sorted_and_deduped() {
    // Breaks on any extra key, key reorder, or surviving duplicate: the
    // worker matches this shape verbatim.
    assert_eq!(
        guest_allow_body(&[222, 7, 7], &[]).to_string(),
        r#"{"invites":[],"t":"guest_allow","uids":[7,222]}"#
    );
    // Empty is a present deny-all, never an absent list — and `invites` is
    // always present too.
    assert_eq!(
        guest_allow_body(&[], &[]).to_string(),
        r#"{"invites":[],"t":"guest_allow","uids":[]}"#
    );
    // Invites sort by hash and dedupe, like uids do.
    let invites = vec![
        PublishedInvite {
            code_hash: "bb".to_string(),
            uid: 9,
            expires: 200,
        },
        PublishedInvite {
            code_hash: "aa".to_string(),
            uid: 7,
            expires: 100,
        },
        PublishedInvite {
            code_hash: "aa".to_string(),
            uid: 7,
            expires: 100,
        },
    ];
    assert_eq!(
        guest_allow_body(&[9], &invites).to_string(),
        r#"{"invites":[{"code_hash":"aa","expires":100,"uid":7},{"code_hash":"bb","expires":200,"uid":9}],"t":"guest_allow","uids":[9]}"#
    );
}

#[tokio::test]
async fn empty_trust_store_publishes_an_empty_deny_all_list() {
    let (_db_dir, db) = test_db();
    let (_scratch, runtime) = allowlist_runtime(&db);
    // The join always publishes, even with no trusted guests ...
    let body = runtime
        .maybe_guest_allow(true)
        .await
        .expect("the join always publishes");
    assert_eq!(body["t"], "guest_allow");
    assert_eq!(body["uids"], json!([]));
    // ... and the empty store republishes nothing until a grant lands.
    assert_eq!(runtime.maybe_guest_allow(false).await, None);
}

#[tokio::test]
async fn allowlist_republishes_only_when_trust_membership_changes() {
    let (_db_dir, db) = test_db();
    let (_scratch, runtime) = allowlist_runtime(&db);
    assert_eq!(allowlist_fingerprint(&db), "");

    runtime.seed_uid_cache("a", 1);
    db.upsert_trust("a", true).expect("trust");
    assert_eq!(allowlist_fingerprint(&db), "a");
    let first = runtime
        .maybe_guest_allow(false)
        .await
        .expect("a grant add publishes");
    assert_eq!(first["uids"], json!([1]));
    assert_eq!(
        runtime.maybe_guest_allow(false).await,
        None,
        "no change, no publish"
    );

    runtime.seed_uid_cache("b", 2);
    db.upsert_trust("b", false).expect("trust");
    assert_eq!(allowlist_fingerprint(&db), "a,b");
    let second = runtime
        .maybe_guest_allow(false)
        .await
        .expect("a second grant add republishes");
    assert_eq!(second["uids"], json!([1, 2]));

    assert!(db.remove_trust("a").expect("remove"));
    assert_eq!(allowlist_fingerprint(&db), "b");
    let third = runtime
        .maybe_guest_allow(false)
        .await
        .expect("a grant remove republishes");
    assert_eq!(third["uids"], json!([2]));

    // A grants-only edit keeps the same logins (and uids): nothing to send.
    db.set_grants("b", &["alpha".to_string()]).expect("grants");
    assert_eq!(allowlist_fingerprint(&db), "b");
    assert_eq!(
        runtime.maybe_guest_allow(false).await,
        None,
        "session edits keep the same uids"
    );
}

/// Mock GitHub `GET /users/{login}`: `alice` resolves to 101, `boom` is a
/// 500, anything else a 404. Returns the base URL and the hit counter.
async fn spawn_uid_mock() -> (String, Arc<AtomicU32>) {
    let hits = Arc::new(AtomicU32::new(0));
    let app = Router::new().route(
        "/users/{login}",
        get({
            let hits = Arc::clone(&hits);
            move |axum::extract::Path(login): axum::extract::Path<String>| {
                let hits = Arc::clone(&hits);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    match login.as_str() {
                        "alice" => (StatusCode::OK, Json(json!({"login": "alice", "id": 101})))
                            .into_response(),
                        "boom" => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error": "boom"})),
                        )
                            .into_response(),
                        _ => (StatusCode::NOT_FOUND, Json(json!({"message": "Not Found"})))
                            .into_response(),
                    }
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the mock GitHub");
    let base = format!("http://{}", listener.local_addr().expect("mock addr"));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, hits)
}

#[tokio::test]
async fn uid_resolution_caches_hits_and_skips_failures() {
    let (base, hits) = spawn_uid_mock().await;
    let (_db_dir, db) = test_db();
    db.upsert_trust("alice", true).expect("trust");
    db.upsert_trust("boom", true).expect("trust");
    db.upsert_trust("ghost", true).expect("trust");
    let cache = Mutex::new(HashMap::new());

    // The 404 and the 500 skip with a warning; they never abort the list.
    let uids = guest_allow::resolve_guest_uids(&db, &cache, "test-Bearer [REDACTED]", &base).await;
    assert_eq!(uids, vec![101]);
    assert_eq!(hits.load(Ordering::SeqCst), 3);

    // Alice serves from the cache now; only the failures hit the network.
    let again = guest_allow::resolve_guest_uids(&db, &cache, "test-Bearer [REDACTED]", &base).await;
    assert_eq!(again, vec![101]);
    assert_eq!(hits.load(Ordering::SeqCst), 5);
}

// ── uploads (protocol v2 `shelf_put_*`) ───────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Send `shelf_put_begin` and return its answer frame.
async fn put_begin(
    bridge: &mut RemoteBridge,
    out: &crate::sessions::ws::OutTx,
    rx: &mut crate::sessions::outbound::ConnRx,
    req: &str,
    id: Option<&str>,
    name: &str,
    bytes: &[u8],
) -> Value {
    let frame = shelf_put_begin_frame(req, id, name, bytes.len() as u64, None, &sha256_hex(bytes));
    bridge.handle_text(&frame, out).await;
    recv_frame(rx).await
}

#[test]
fn resolve_upload_is_owner_true_and_guest_default_deny() {
    let (_dir, db) = test_db();
    assert!(resolve_upload(&db, OWNER, OWNER));
    assert!(!resolve_upload(&db, OWNER, GUEST), "default deny");
    assert!(!resolve_upload(&db, OWNER, "  "));
    db.upsert_trust(GUEST, true).expect("trust");
    assert!(
        !resolve_upload(&db, OWNER, GUEST),
        "all_sessions grants no upload"
    );
    db.set_upload(GUEST, true).expect("upload grant");
    assert!(resolve_upload(&db, OWNER, GUEST));
}

#[tokio::test]
async fn guest_without_upload_grant_is_forbidden_at_begin() {
    let hub = grant_hub();
    let (out, mut out_rx) = crate::sessions::outbound::channel();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, false);

    let answer = put_begin(
        &mut bridge,
        &out,
        &mut out_rx,
        "u1",
        Some(&hub.alpha.id),
        "x.md",
        b"x",
    )
    .await;
    assert_eq!(answer["t"], "error");
    assert_eq!(answer["req"], "u1");
    assert_eq!(answer["code"], ERR_FORBIDDEN);
    assert!(hub.state.shelf.list(Some(&hub.alpha.id)).is_empty());
}

#[tokio::test]
async fn guest_with_upload_is_allowed_only_into_the_granted_session() {
    let hub = grant_hub();
    let (out, mut out_rx) = crate::sessions::outbound::channel();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, true);
    let bytes = b"user upload".to_vec();

    let ready = put_begin(
        &mut bridge,
        &out,
        &mut out_rx,
        "u1",
        Some(&hub.alpha.id),
        "notes.md",
        &bytes,
    )
    .await;
    assert_eq!(ready["t"], "shelf_put_ready", "got {ready}");
    let file_id = ready["file_id"].as_str().expect("file id").to_string();
    bridge
        .handle_text(&shelf_put_part_frame(&file_id, 0, &bytes), &out)
        .await;
    bridge
        .handle_text(&shelf_put_end_frame(&file_id), &out)
        .await;

    let row = hub.state.shelf.get(&file_id).expect("row");
    assert_eq!(row.session_id.as_deref(), Some(hub.alpha.id.as_str()));
    assert_eq!(row.origin, "user");
    assert_eq!(hub.state.shelf.read_file(&file_id).unwrap(), bytes);
    let contents = hub.alpha.test_contents().unwrap_or_default();
    let shelf_path = hub.state.shelf.path_for(&file_id).expect("shelf path");
    let expected = format!(
        "📎 the user attached `notes.md` ({:.1} MB) → {}",
        bytes.len() as f64 / 1_000_000.0,
        shelf_path.display()
    );
    assert!(
        contents.contains(&expected),
        "the upload appends the exact session line, got: {contents:?}"
    );

    // The ungranted session and the host shelf both refuse.
    let denied = put_begin(
        &mut bridge,
        &out,
        &mut out_rx,
        "u2",
        Some(&hub.beta.id),
        "x.md",
        b"x",
    )
    .await;
    assert_eq!(denied["code"], ERR_FORBIDDEN, "got {denied}");
    let host = put_begin(&mut bridge, &out, &mut out_rx, "u3", None, "x.md", b"x").await;
    assert_eq!(host["code"], ERR_FORBIDDEN, "got {host}");
}

#[tokio::test]
async fn a_fourth_concurrent_upload_is_refused_busy() {
    let hub = grant_hub();
    let (out, mut out_rx) = crate::sessions::outbound::channel();
    let mut bridge = RemoteBridge::new(Arc::clone(&hub.state));

    for index in 0..3 {
        let ready = put_begin(
            &mut bridge,
            &out,
            &mut out_rx,
            &format!("u{index}"),
            None,
            &format!("f{index}.bin"),
            b"x",
        )
        .await;
        assert_eq!(ready["t"], "shelf_put_ready", "begin {index}: {ready}");
    }
    let refused = put_begin(&mut bridge, &out, &mut out_rx, "u3", None, "f3.bin", b"x").await;
    assert_eq!(refused["t"], "error");
    assert_eq!(refused["code"], ERR_BUSY);
}

#[tokio::test]
async fn a_part_or_end_after_the_grant_vanishes_is_forbidden_and_drops_the_temp() {
    let hub = grant_hub();
    let (out, mut out_rx) = crate::sessions::outbound::channel();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant, true);
    let bytes = [1u8, 2u8];

    // Two uploads: one mid-parts, one with every part already sent.
    let ready = put_begin(
        &mut bridge,
        &out,
        &mut out_rx,
        "u1",
        Some(&hub.alpha.id),
        "x.bin",
        &bytes,
    )
    .await;
    let part_id = ready["file_id"].as_str().expect("file id").to_string();
    bridge
        .handle_text(&shelf_put_part_frame(&part_id, 0, &bytes[..1]), &out)
        .await;
    let ready = put_begin(
        &mut bridge,
        &out,
        &mut out_rx,
        "u2",
        Some(&hub.alpha.id),
        "y.bin",
        &bytes,
    )
    .await;
    let end_id = ready["file_id"].as_str().expect("file id").to_string();
    for (part, chunk) in bytes.chunks(1).enumerate() {
        bridge
            .handle_text(&shelf_put_part_frame(&end_id, part as u64, chunk), &out)
            .await;
    }

    // The grant covers a live session; killing it removes the target.
    hub.state.hub.kill(&hub.alpha.id);

    bridge
        .handle_text(&shelf_put_part_frame(&part_id, 1, &bytes[1..]), &out)
        .await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_FORBIDDEN, "got {denied}");
    assert_dropped(&hub, &part_id);

    bridge
        .handle_text(&shelf_put_end_frame(&end_id), &out)
        .await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_FORBIDDEN, "got {denied}");
    assert_dropped(&hub, &end_id);
}

/// An upload dropped mid-flight leaves no state, no temp and no row.
fn assert_dropped(hub: &GrantHub, file_id: &str) {
    assert!(
        hub.state.shelf.upload_session(file_id).is_none(),
        "the in-flight upload is dropped"
    );
    let temp = hub.state.shelf.dir().join(format!(".{file_id}.upload"));
    assert!(!temp.exists(), "and its temp bytes are deleted");
    assert!(
        hub.state.shelf.get(file_id).is_none(),
        "and no row is committed"
    );
}
