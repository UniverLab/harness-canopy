//! Route-level tests for the guest path: `device_identity` tracking, guest
//! `pair_init` refusal, and guest offer routing through [`RelayRuntime`].

use serde_json::json;

use super::{ConnectionRegistry, RelayRuntime};
use crate::db::Database;
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
            &json!({"t": "device_identity", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            "d:9",
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
            &json!({"t": "device_identity", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            "d:9",
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
            &json!({"t": "device_identity", "role": "guest", "login": "mallory", "uid": 42}),
            0,
            "d:9",
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
            &json!({"t": "device_identity", "role": "device", "login": "octocat", "uid": 7}),
            0,
            "d:1",
            &signal,
        ),
        None
    );
    // So does a guest notice without identity.
    assert_eq!(
        runtime.route(
            &json!({"t": "device_identity", "role": "guest"}),
            0,
            "d:2",
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
