//! Route-level tests for the guest path: `device_identity` tracking, guest
//! `pair_init` refusal, and guest offer routing through [`RelayRuntime`].

use base64::Engine as _;
use serde_json::json;

use super::{ConnectionRegistry, RelayRuntime, SignalTx};
use crate::db::Database;
use crate::remote::guest::{attested_body, RELAY_FROM};
use crate::remote::hostkey::HostKey;
use crate::remote::pairing::PairingState;
use crate::remote::signaling::guest_offer_message_text;

type Inbox = tokio::sync::mpsc::UnboundedReceiver<(String, serde_json::Value)>;

/// The offer clock both sides of these tests agree on.
const NOW_TS: i64 = 1_800_000_000;

/// Deterministic guest key (scalar 0x33): signs offers, binds as `pubkey`.
fn guest_key() -> HostKey {
    HostKey::from_scalar_bytes(&[0x33; 32]).expect("guest key")
}

/// A key the guest never holds: a relay forgery or a second device.
fn other_key() -> HostKey {
    HostKey::from_scalar_bytes(&[0x44; 32]).expect("other key")
}

/// This runtime's host id: [`runtime`] builds its key from scalar 0x11.
fn host_id() -> String {
    HostKey::from_scalar_bytes(&[0x11; 32])
        .expect("host key")
        .host_id()
}

/// One guest offer body signed by `key`; `pubkey` rides only when `Some`.
fn signed_body(
    host: &str,
    login: &str,
    uid: u64,
    ts: i64,
    sdp: &str,
    key: &HostKey,
    pubkey: Option<&str>,
) -> serde_json::Value {
    let message = guest_offer_message_text(host, login, uid, &ts.to_string(), sdp);
    let sig = base64::engine::general_purpose::STANDARD.encode(key.sign(message.as_bytes()));
    let mut body = json!({
        "t": "offer", "login": login, "uid": uid, "ts": ts, "sdp": sdp, "sig": sig,
    });
    if let Some(spki) = pubkey {
        body["pubkey"] = json!(spki);
    }
    body
}

/// Track `d:<conn>` as the relay's guest connection for `login`/`uid`.
fn track_guest(runtime: &RelayRuntime, signal: &SignalTx, conn: &str, login: &str, uid: u64) {
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": conn, "role": "guest",
                    "login": login, "uid": uid}),
            NOW_TS,
            RELAY_FROM,
            signal,
        ),
        None,
        "tracking answers nothing"
    );
}

/// Offer `body` on the tracked `conn` and read the refusal off the channel.
fn offer(
    runtime: &RelayRuntime,
    signal: &SignalTx,
    inbox: &mut Inbox,
    conn: &str,
    body: &serde_json::Value,
) -> serde_json::Value {
    assert_eq!(
        runtime.route(body, NOW_TS, conn, signal),
        None,
        "offers answer over the signal channel"
    );
    let (to, refusal) = inbox.try_recv().expect("a refusal on the signal channel");
    assert_eq!(to, conn);
    refusal
}

fn test_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    crate::test_support::hold(dir);
    db
}

fn runtime(db: &Database, dir: &tempfile::TempDir) -> RelayRuntime {
    RelayRuntime::new(
        HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key"),
        db.clone(),
        "owner".to_string(),
        PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    )
}

#[test]
fn guest_pair_init_fails_like_a_wrong_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let runtime = runtime(&db, &dir);
    let (signal, _) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "9", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            RELAY_FROM,
            &signal,
        ),
        None,
        "tracking answers nothing"
    );
    // A guest `pair_init` never reaches pairing state: `bad_code`, no new
    // wire string.
    let reply = runtime
        .route(&json!({"t": "pair_init"}), 0, "d:9", &signal)
        .expect("an inline reply");
    assert_eq!(reply, json!({"t": "pair_fail", "reason": "bad_code"}));
    // A device connection still reaches pairing (no code live: `no_code`).
    let reply = runtime
        .route(&json!({"t": "pair_init"}), 0, "d:1", &signal)
        .expect("an inline reply");
    assert_eq!(reply, json!({"t": "pair_fail", "reason": "no_code"}));
}

#[test]
fn guest_offer_without_injected_identity_is_refused_forbidden() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "9", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            RELAY_FROM,
            &signal,
        ),
        None
    );
    assert_eq!(
        runtime.route(
            &json!({"t": "offer", "github_login": "mallory", "sdp": "v=0"}),
            0,
            "d:9",
            &signal,
        ),
        None,
        "a refused offer answers over the signal channel"
    );
    let (to, refusal) = inbox.try_recv().expect("the refusal");
    assert_eq!(to, "d:9");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "forbidden"}));
}

#[test]
fn guest_offer_with_grant_redeems_the_invite_before_failing_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let issued = crate::remote::invite::issue(&db, "mallory", 42, 1_800_000_000, 7).expect("issue");
    assert_eq!(issued.code.len(), 23);
    let runtime = runtime(&db, &dir);
    // No sessions attached: the verified offer cannot spawn a peer.
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "9", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            RELAY_FROM,
            &signal,
        ),
        None
    );
    assert_eq!(
        runtime.route(
            &json!({"t": "offer", "login": "mallory", "uid": 42, "sdp": "v=0"}),
            1_800_000_000,
            "d:9",
            &signal,
        ),
        None
    );
    let (to, refusal) = inbox.try_recv().expect("the refusal");
    assert_eq!(to, "d:9");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "busy"}));
    // The first accepted offer already redeemed the invite: single-use.
    assert!(
        crate::remote::invite::live(&db, 1_800_000_000).is_empty(),
        "the accepted offer burns the invite even when no peer spawns"
    );
    assert!(
        db.list_invites().expect("list")[0].used,
        "the row stays, marked used"
    );
}

#[test]
fn non_guest_device_identity_is_not_tracked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    // A device notice leaves the connection on the device path.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "1", "role": "device", "login": "octocat", "uid": 7}),
            0,
            RELAY_FROM,
            &signal,
        ),
        None
    );
    // So does a guest notice without identity.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "2", "role": "guest"}),
            0,
            RELAY_FROM,
            &signal
        ),
        None
    );
    for from in ["d:1", "d:2"] {
        assert_eq!(
            runtime.route(
                &json!({"t": "offer", "login": "octocat", "uid": 7, "sdp": "v=0"}),
                0,
                from,
                &signal,
            ),
            None
        );
        let (to, refusal) = inbox.try_recv().expect("the refusal");
        assert_eq!(to, from);
        assert_eq!(
            refusal,
            json!({"t": "connect_fail", "reason": "unknown_device"}),
            "untracked connections keep the device verdict"
        );
    }
}

#[test]
fn guest_notice_to_attested_offer_roundtrip() {
    const NOW: i64 = 1_800_000_000;
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    crate::remote::invite::issue(&db, "mallory", 42, NOW, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    // One envelope through the socket's path: attested, then routed.
    let mut send = |envelope: &serde_json::Value| {
        let body = attested_body(envelope).expect("every envelope here carries a body");
        let from = envelope["from"]
            .as_str()
            .expect("every envelope here names its from");
        assert_eq!(
            runtime.route(&body, NOW, from, &signal),
            None,
            "offers answer over the signal channel"
        );
        inbox.try_recv().expect("a refusal on the signal channel")
    };
    // (a) notice, then attested offer: verified. `busy` is the verified
    // verdict here: no sessions in a unit test, same as the grant test.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "c-1", "role": "guest", "login": "mallory", "uid": 42}),
            NOW,
            RELAY_FROM,
            &signal,
        ),
        None,
        "tracking answers nothing"
    );
    let (to, refusal) = send(
        &json!({"from": "d:c-1", "login": "mallory", "uid": 42, "body": {"t": "offer", "sdp": "v=0"}}),
    );
    assert_eq!(to, "d:c-1");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "busy"}));
    assert!(
        crate::remote::invite::live(&db, NOW).is_empty(),
        "the verified offer burns the invite"
    );
    // (b) the same offer from a connection the relay never announced: an
    // untracked `from` falls through to the device path and is refused.
    let (to, refusal) = send(
        &json!({"from": "d:c-9", "login": "mallory", "uid": 42, "body": {"t": "offer", "sdp": "v=0"}}),
    );
    assert_eq!(to, "d:c-9");
    assert_eq!(
        refusal,
        json!({"t": "connect_fail", "reason": "unknown_device"})
    );
    // (c) `device_left` forgets the connection: the same offer is refused.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_left", "conn": "c-1"}),
            NOW,
            RELAY_FROM,
            &signal
        ),
        None
    );
    let (to, refusal) = send(
        &json!({"from": "d:c-1", "login": "mallory", "uid": 42, "body": {"t": "offer", "sdp": "v=0"}}),
    );
    assert_eq!(to, "d:c-1");
    assert_eq!(
        refusal,
        json!({"t": "connect_fail", "reason": "unknown_device"})
    );
    // (d) a body claiming another login still verifies against the attested
    // one: the body's login/uid alone would forbid on the uid mismatch.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "conn": "c-2", "role": "guest", "login": "mallory", "uid": 42}),
            NOW,
            RELAY_FROM,
            &signal,
        ),
        None
    );
    let (to, refusal) = send(&json!({"from": "d:c-2", "login": "mallory", "uid": 42,
                "body": {"t": "offer", "login": "owner", "uid": 1, "sdp": "v=0"}}));
    assert_eq!(to, "d:c-2");
    assert_eq!(
        refusal,
        json!({"t": "connect_fail", "reason": "busy"}),
        "the attested identity won, not the body's"
    );
    // (e) a bare device frame, no attestation: refused on the guest path.
    let (to, refusal) = send(&json!({"from": "d:c-2", "body": {"t": "offer", "sdp": "v=0"}}));
    assert_eq!(to, "d:c-2");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "forbidden"}));
}

#[test]
fn keyed_guest_signed_offer_verifies_and_replay_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let key = guest_key();
    db.set_guest_pubkey("mallory", &key.spki_b64())
        .expect("key");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let host = host_id();
    let body = signed_body(&host, "mallory", 42, NOW_TS, "v=0", &key, None);
    // Verified: no sessions in a unit test, so `busy` is the accept verdict.
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &body),
        json!({"t": "connect_fail", "reason": "busy"}),
        "a valid signed offer is accepted"
    );
    // The identical bytes again: the signature is spent.
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &body),
        json!({"t": "connect_fail", "reason": "bad_signature"}),
        "a replayed signature is refused"
    );
}

#[test]
fn keyed_guest_forgeries_are_refused_and_burn_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let key = guest_key();
    db.set_guest_pubkey("mallory", &key.spki_b64())
        .expect("key");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let host = host_id();
    let bad = json!({"t": "connect_fail", "reason": "bad_signature"});
    // Signed "v=0", sent "v=1": the signature pins the SDP.
    let mut tampered = signed_body(&host, "mallory", 42, NOW_TS, "v=0", &key, None);
    tampered["sdp"] = json!("v=1");
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &tampered), bad);
    // Signed for another login, relabeled to the tracked one.
    let mut relabeled = signed_body(&host, "mallory2", 42, NOW_TS, "v=0", &key, None);
    relabeled["login"] = json!("mallory");
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &relabeled), bad);
    // Signed for another host: the relay's room is not this host.
    let foreign = signed_body("deadbeefdeadbeef", "mallory", 42, NOW_TS, "v=0", &key, None);
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &foreign), bad);
    // The relay's key, not the guest's: a relay forgery.
    let forged = signed_body(&host, "mallory", 42, NOW_TS, "v=0", &other_key(), None);
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &forged), bad);
    // No signature at all (nor timestamp) on a keyed login.
    let unsigned = json!({"t": "offer", "login": "mallory", "uid": 42, "sdp": "v=0"});
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &unsigned), bad);
    // Malformed signature bytes.
    let mut mangled = signed_body(&host, "mallory", 42, NOW_TS, "v=0", &key, None);
    mangled["sig"] = json!("!!!not-base64!!!");
    assert_eq!(offer(&runtime, &signal, &mut inbox, "d:9", &mangled), bad);
    // Fresh signature, ancient timestamp: the window still applies.
    let stale = signed_body(&host, "mallory", 42, NOW_TS - 1_000, "v=0", &key, None);
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &stale),
        json!({"t": "connect_fail", "reason": "stale"}),
        "a ts outside the window reads stale"
    );
    // A body login the relay never attested for this connection.
    let wrong_login = signed_body(&host, "owner", 42, NOW_TS, "v=0", &key, None);
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &wrong_login),
        json!({"t": "connect_fail", "reason": "forbidden"}),
        "identity mismatch reads forbidden before any crypto"
    );
    // Every refusal above left the invite alone: only an accepted offer
    // redeems it.
    assert_eq!(
        crate::remote::invite::live(&db, NOW_TS).len(),
        1,
        "refusals burn nothing"
    );
}

#[test]
fn keyless_guest_unsigned_offer_is_accepted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let body = json!({"t": "offer", "login": "mallory", "uid": 42, "sdp": "v=0"});
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &body),
        json!({"t": "connect_fail", "reason": "busy"}),
        "a keyless guest trusts the relay: no signature needed"
    );
}

#[test]
fn valid_proof_on_redeeming_offer_registers_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let host = host_id();
    let key = guest_key();
    let redeeming = signed_body(
        &host,
        "mallory",
        42,
        NOW_TS,
        "v=0",
        &key,
        Some(&key.spki_b64()),
    );
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &redeeming),
        json!({"t": "connect_fail", "reason": "busy"}),
        "the redeeming offer is accepted"
    );
    assert_eq!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .as_deref(),
        Some(key.spki_b64()).as_deref(),
        "proof of possession binds the key"
    );
    // Now keyed: an unsigned offer is refused, a fresh signed one passes.
    let unsigned = json!({"t": "offer", "login": "mallory", "uid": 42, "sdp": "v=1"});
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &unsigned),
        json!({"t": "connect_fail", "reason": "bad_signature"}),
        "a keyed guest must sign every offer"
    );
    let next = signed_body(&host, "mallory", 42, NOW_TS, "v=1", &key, None);
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &next),
        json!({"t": "connect_fail", "reason": "busy"}),
        "a fresh signature under the bound key is accepted"
    );
}

#[test]
fn redeeming_offer_without_proof_binds_nothing_later_offers_cannot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let host = host_id();
    let key = guest_key();
    // Unsigned but carrying a key: accepted (keyless) yet binds nothing,
    // because there is no proof of possession.
    let bare = json!({
        "t": "offer", "login": "mallory", "uid": 42, "sdp": "v=0",
        "pubkey": key.spki_b64(),
    });
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &bare),
        json!({"t": "connect_fail", "reason": "busy"})
    );
    assert!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .is_none(),
        "no proof, no bind"
    );
    // The invite is spent: even a valid proof now arrives too late.
    let late = signed_body(
        &host,
        "mallory",
        42,
        NOW_TS,
        "v=1",
        &key,
        Some(&key.spki_b64()),
    );
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &late),
        json!({"t": "connect_fail", "reason": "busy"}),
        "still accepted: the login is still keyless"
    );
    assert!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .is_none(),
        "only the redeeming offer binds"
    );
}

#[test]
fn registered_key_is_never_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let bound = guest_key();
    db.set_guest_pubkey("mallory", &bound.spki_b64())
        .expect("key");
    crate::remote::invite::issue(&db, "mallory", 42, NOW_TS, 7).expect("issue");
    let runtime = runtime(&db, &dir);
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    track_guest(&runtime, &signal, "9", "mallory", 42);
    let host = host_id();
    let rival = other_key();
    // Signed by the bound key (so it verifies) but carrying a rival key:
    // accepted, yet the stored key does not move.
    let squat = signed_body(
        &host,
        "mallory",
        42,
        NOW_TS,
        "v=0",
        &bound,
        Some(&rival.spki_b64()),
    );
    assert_eq!(
        offer(&runtime, &signal, &mut inbox, "d:9", &squat),
        json!({"t": "connect_fail", "reason": "busy"})
    );
    assert_eq!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .as_deref(),
        Some(bound.spki_b64()).as_deref(),
        "a registered key is never overwritten"
    );
}

#[test]
fn trust_add_keeps_key_and_remove_drops_it() {
    let db = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let key = guest_key();
    db.set_guest_pubkey("mallory", &key.spki_b64())
        .expect("key");
    crate::remote::panel::trust_add(&db, "mallory", true, &[], false).expect("trust_add");
    assert_eq!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .as_deref(),
        Some(key.spki_b64()).as_deref(),
        "re-granting keeps the key"
    );
    assert!(
        db.remove_trust("mallory").expect("remove"),
        "a row was removed"
    );
    assert!(
        db.get_trust("mallory").expect("get").is_none(),
        "removing the trust drops the key with the row"
    );
    db.upsert_trust("mallory", true).expect("re-trust");
    assert!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .is_none(),
        "re-trusting a login starts keyless"
    );
}

#[test]
fn migration_adds_guest_pubkey_column_to_old_databases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("old.db");
    {
        let conn = rusqlite::Connection::open(&path).expect("raw open");
        conn.execute_batch(
            "CREATE TABLE remote_trust_users (
                 login TEXT PRIMARY KEY,
                 all_sessions INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE remote_trust_grants (
                 login TEXT NOT NULL REFERENCES remote_trust_users(login) ON DELETE CASCADE,
                 session_name TEXT NOT NULL,
                 PRIMARY KEY (login, session_name)
             );
             CREATE TABLE remote_invites (
                 code_hash TEXT PRIMARY KEY,
                 login TEXT NOT NULL,
                 uid INTEGER NOT NULL,
                 expires_at INTEGER NOT NULL,
                 used INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO remote_trust_users (login, all_sessions) VALUES ('mallory', 1);",
        )
        .expect("old schema");
    }
    let db = Database::new(&path).expect("open migrates");
    assert!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .is_none(),
        "an old row reads back keyless"
    );
    db.set_guest_pubkey("mallory", "a2V5").expect("set");
    assert_eq!(
        db.get_trust("mallory")
            .expect("get")
            .expect("trusted")
            .guest_pubkey
            .as_deref(),
        Some("a2V5"),
        "the migrated column stores keys"
    );
}
