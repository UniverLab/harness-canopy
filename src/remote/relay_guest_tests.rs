//! Route-level tests for the guest path: `device_identity` tracking, guest
//! `pair_init` refusal, and guest offer routing through [`RelayRuntime`].

use serde_json::json;

use super::{ConnectionRegistry, RelayRuntime};
use crate::db::Database;
use crate::remote::guest::{attested_body, RELAY_FROM};
use crate::remote::hostkey::HostKey;
use crate::remote::pairing::PairingState;

fn test_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    std::mem::forget(dir);
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
