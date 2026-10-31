//! The grant matrix: who sees which sessions, and what revoking does.
//!
//! Untrusted logins resolve to nothing and offers from them are refused;
//! the owner resolves to [`Grant::All`] with no trust row; a full grant
//! sees every agent session; a partial grant lists only its subset and
//! meets `not_found` everywhere else; removing or narrowing a profile
//! kicks live guest peers with a farewell. Each test breaks when its
//! production check is skipped: the pairing bind, the bridge filter, or
//! the kick.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use super::{normalize_login, resolve, Grant};
use crate::application::ports::StateRepository as _;
use crate::db::Database;
use crate::remote::bridge::RemoteBridge;
use crate::remote::devices::{device_id, RemoteDevices};
use crate::remote::hostkey::HostKey;
use crate::remote::pairing::{derive_k_pair, mac_init, normalize_code, PairingState};
use crate::remote::panel;
use crate::remote::relay::{ConnectionRegistry, RelayRuntime, REMOTE_TRUST_REVOKE_KEY};
use crate::remote::signaling::{offer_message, verify_offer, Refusal};
use crate::sessions::protocol::{
    attach_frame, list_frame, shelf_delete_frame, shelf_get_frame, shelf_list_frame, ERR_FORBIDDEN,
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

async fn recv_frame(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Value {
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

    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant);
    bridge.handle_text(&list_frame("r1"), &out_tx).await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(list["t"], "sessions");
    let names = session_names(&list);
    assert!(names.contains(&"alpha".to_string()), "{names:?}");
    assert!(names.contains(&"beta".to_string()), "{names:?}");
    assert!(
        !names.contains(&"shell".to_string()),
        "terminals stay hidden even for a full grant: {names:?}"
    );

    bridge
        .handle_text(&attach_frame(&hub.beta.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    assert_eq!(snapshot["id"], hub.beta.id);
    bridge.abort();
}

#[tokio::test]
async fn partial_grant_lists_only_the_subset_and_hides_the_rest() {
    let hub = grant_hub();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant);

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

    // The granted session still attaches.
    bridge
        .handle_text(&attach_frame(&hub.alpha.id), &out_tx)
        .await;
    let snapshot = recv_frame(&mut out_rx).await;
    assert_eq!(snapshot["t"], "snapshot");
    bridge.abort();
}

#[test]
fn partial_grant_filters_lifecycle_events_to_its_names() {
    let hub = grant_hub();
    let grant = Grant::Sessions(HashSet::from(["alpha".to_string()]));
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant);

    bridge.handle_event(HubEvent::SessionAdded(Box::new(hub.beta.item())), &out_tx);
    assert!(
        out_rx.try_recv().is_err(),
        "an ungranted arrival must not forward"
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

#[test]
fn pairing_binds_the_attested_login_and_refuses_untrusted_guests() {
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
    panel::trust_add(&db, GUEST, false, &["alpha".to_string()]).expect("narrow");
    let queue = db
        .get_state(REMOTE_TRUST_REVOKE_KEY)
        .expect("queue read")
        .unwrap_or_default();
    assert!(queue.contains(GUEST), "a narrowed grant kicks: {queue}");

    // Scoped-to-all widens: nothing is queued.
    db.set_state(REMOTE_TRUST_REVOKE_KEY, "").expect("clear");
    panel::trust_add(&db, GUEST, true, &[]).expect("widen");
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
    )
    .expect("scoped");
    db.set_state(REMOTE_TRUST_REVOKE_KEY, "").expect("clear");
    panel::trust_add(&db, GUEST, false, &["alpha".to_string()]).expect("drop");
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
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), grant);

    // The unscoped list is the host shelf, visible to every peer.
    bridge
        .handle_text(&shelf_list_frame("s1", None), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(list["t"], "shelf");
    assert_eq!(shelf_names(&list), vec!["host.png".to_string()]);

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
    for file_id in [&beta_file, &shell_file, &orphan_file] {
        bridge
            .handle_text(&shelf_get_frame("s4", file_id), &out_tx)
            .await;
        let denied = recv_frame(&mut out_rx).await;
        assert_eq!(denied["t"], "error");
        assert_eq!(denied["code"], ERR_NOT_FOUND, "{denied}");
    }
    bridge
        .handle_text(&shelf_delete_frame("s5", &beta_file), &out_tx)
        .await;
    let denied = recv_frame(&mut out_rx).await;
    assert_eq!(denied["code"], ERR_NOT_FOUND, "{denied}");

    // The granted file and the host file still serve.
    for file_id in [&alpha_file, &host_file] {
        bridge
            .handle_text(&shelf_get_frame("s6", file_id), &out_tx)
            .await;
        let file = recv_frame(&mut out_rx).await;
        assert_eq!(file["t"], "file", "{file}");
    }
    bridge.abort();
}

#[tokio::test]
async fn full_grant_serves_every_shelf_and_keeps_the_terminal_rule() {
    let hub = grant_hub();
    let beta_file = shelve(&hub.state, "beta.png", Some(&hub.beta.id));
    let shell_file = shelve(&hub.state, "shell.png", Some(&hub.shell.id));
    let orphan_file = shelve(&hub.state, "orphan.png", Some("no-such-session"));

    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut bridge = RemoteBridge::with_grant(Arc::clone(&hub.state), Grant::All);

    // A scoped list reaches its session's files.
    bridge
        .handle_text(&shelf_list_frame("s1", Some(hub.beta.id.as_str())), &out_tx)
        .await;
    let list = recv_frame(&mut out_rx).await;
    assert_eq!(shelf_names(&list), vec!["beta.png".to_string()]);

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
