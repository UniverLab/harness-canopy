//! Invite code and guest-offer tests (audit finding N3).

use serde_json::json;

use super::{
    code_hash, guest_link, guest_link_for, issue, live, normalize_code_input, NoGuestLink,
};
use crate::db::Database;
use crate::remote::guest::GuestConns;
use crate::remote::signaling::{connect_fail, verify_offer, Refusal};
use crate::remote::trust;

const NOW: i64 = 1_800_000_000;

fn test_db() -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    (dir, db)
}

fn guest_notice(login: &str, uid: u64) -> serde_json::Value {
    json!({"t": "device_identity", "role": "guest", "login": login, "uid": uid})
}

#[test]
fn invite_issue_redeem_roundtrip() {
    let (_dir, db) = test_db();
    let issued = issue(&db, "octocat", 7, NOW, 7).expect("issue");
    assert_eq!(issued.code.len(), 23, "CNR- plus 16 symbols plus 3 dashes");
    assert!(issued.code.starts_with("CNR-"), "{}", issued.code);
    assert_eq!(issued.expires_at, NOW + 7 * 86_400);
    // Stored as a hash only: the row carries SHA256(canonical), never the
    // cleartext — without hashing, the stored value cannot mint the door.
    let rows = db.list_invites().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].code_hash, code_hash(&issued.code));
    assert_eq!(rows[0].code_hash.len(), 64);
    assert_eq!(rows[0].login, "octocat");
    assert_eq!(rows[0].uid, 7);
    assert!(!rows[0].used);
    // Live right after issue, and the publish frame carries its hash.
    let published = live(&db, NOW + 60);
    assert_eq!(published.len(), 1);
    let body = trust::guest_allow_body(&[7], &published);
    assert_eq!(body["invites"][0]["code_hash"], json!(rows[0].code_hash));
    assert_eq!(body["invites"][0]["uid"], json!(7));
    // The first redemption burns the code: single-use, gone from the next
    // publish, and a second burn finds nothing left.
    assert_eq!(db.consume_invites_for_uid(7, NOW + 60).expect("consume"), 1);
    assert!(live(&db, NOW + 60).is_empty());
    assert_eq!(
        trust::guest_allow_body(&[7], &live(&db, NOW + 60))["invites"],
        json!([])
    );
    assert_eq!(db.consume_invites_for_uid(7, NOW + 60).expect("consume"), 0);
}

#[test]
fn guest_offer_uses_injected_identity_not_body() {
    let (_dir, db) = test_db();
    db.upsert_trust("mallory", false).expect("trust");
    db.set_grants("mallory", &["mallory-room".to_string()])
        .expect("grants");
    let guests = GuestConns::default();
    guests.note_device_identity(&guest_notice("mallory", 42), "d:9");
    // The body spoofs a more-privileged login plus a device row and a
    // signature that are not hers; the host must ignore all of it and
    // grant by the relay-injected identity.
    let body = json!({
        "t": "offer",
        "login": "mallory", "uid": 42,
        "github_login": "owner", "device_id": "bfcba2b742a807bb",
        "sig": "bogus", "ts": NOW, "sdp": "v=0",
    });
    let offer = guests
        .verify_guest_offer(&db, "owner", &body, "d:9")
        .expect("the guest offer verifies");
    assert_eq!(offer.uid, 42);
    assert_eq!(offer.sdp, "v=0");
    assert!(offer.grant.allows("mallory-room"));
    assert!(
        !offer.grant.allows("owner-room"),
        "the spoofed owner login must not widen the grant"
    );
}

#[test]
fn guest_offer_without_injected_identity_rejected() {
    let (_dir, db) = test_db();
    db.upsert_trust("mallory", true).expect("trust");
    let guests = GuestConns::default();
    guests.note_device_identity(&guest_notice("mallory", 42), "d:9");
    // No injected fields at all, only the spoofable body hint.
    let no_identity = json!({"t": "offer", "github_login": "mallory", "sdp": "v=0"});
    assert!(
        matches!(
            guests.verify_guest_offer(&db, "owner", &no_identity, "d:9"),
            Err(Refusal::Forbidden)
        ),
        "an offer without injected identity is forbidden"
    );
    // Half an identity is no identity.
    let no_uid = json!({"t": "offer", "login": "mallory", "sdp": "v=0"});
    assert!(matches!(
        guests.verify_guest_offer(&db, "owner", &no_uid, "d:9"),
        Err(Refusal::Forbidden)
    ));
    // Right login, wrong uid.
    let wrong_uid = json!({"t": "offer", "login": "mallory", "uid": 43, "sdp": "v=0"});
    assert!(matches!(
        guests.verify_guest_offer(&db, "owner", &wrong_uid, "d:9"),
        Err(Refusal::Forbidden)
    ));
    // Right identity on an untracked connection.
    let untracked = json!({"t": "offer", "login": "mallory", "uid": 42, "sdp": "v=0"});
    assert!(matches!(
        guests.verify_guest_offer(&db, "owner", &untracked, "d:10"),
        Err(Refusal::Forbidden)
    ));
    // Injected identity with no grant.
    let (_dir, untrusted) = test_db();
    let strangers = GuestConns::default();
    strangers.note_device_identity(&guest_notice("stranger", 43), "d:11");
    let no_grant = json!({"t": "offer", "login": "stranger", "uid": 43, "sdp": "v=0"});
    assert!(matches!(
        strangers.verify_guest_offer(&untrusted, "owner", &no_grant, "d:11"),
        Err(Refusal::Forbidden)
    ));
    // The wire reads the existing `forbidden`: no new reason string.
    assert_eq!(
        connect_fail(Refusal::Forbidden),
        json!({"t": "connect_fail", "reason": "forbidden"})
    );
}

#[test]
fn expired_invite_rejected() {
    let (_dir, db) = test_db();
    issue(&db, "octocat", 7, NOW, 7).expect("issue");
    assert_eq!(live(&db, NOW + 60).len(), 1, "fresh invites are live");
    // A day past expiry the invite leaves the publish: the relay door it
    // feeds on hashes plus `expires > now` rejects it from then on.
    assert!(
        live(&db, NOW + 8 * 86_400).is_empty(),
        "expired invites leave the publish"
    );
    let body = trust::guest_allow_body(&[7], &live(&db, NOW + 8 * 86_400));
    assert_eq!(body["invites"], json!([]));
}

#[test]
fn device_offer_path_unchanged() {
    use base64::Engine as _;

    use crate::remote::devices::{device_id, RemoteDevices};
    use crate::remote::hostkey::HostKey;

    let (_dir, db) = test_db();
    let devices = RemoteDevices::new(db);
    let device = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    devices
        .pair(
            device.spki_der().to_vec(),
            "browser",
            "octocat",
            "2026-01-01T00:00:00Z",
        )
        .expect("pair");
    let host_id = "76805a63a6980c18";
    let ts = 1_793_100_000;
    let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";
    let message = crate::remote::signaling::offer_message(host_id, ts, sdp);
    let sig = base64::engine::general_purpose::STANDARD.encode(device.sign(message.as_bytes()));
    // A new-relay device offer carries flat injected login/uid fields that
    // the device path ignores: verification still keys on the device row
    // plus the signature, exactly as before.
    let mut body = json!({
        "t": "offer",
        "device_id": device_id(device.spki_der()),
        "ts": ts, "sdp": sdp, "sig": sig, "github_login": "octocat",
    });
    body["login"] = json!("octocat");
    body["uid"] = json!(7);
    let offer = verify_offer(&devices, host_id, &body, ts).expect("device offer verifies");
    assert_eq!(offer.sdp, sdp);
    // Unknown devices fail exactly as before, injected fields or not.
    let mut unknown = body.clone();
    unknown["device_id"] = json!("0000000000000000");
    assert!(matches!(
        verify_offer(&devices, host_id, &unknown, ts),
        Err(Refusal::UnknownDevice)
    ));
    // A mismatched body hint still fails the signature check.
    let mut mismatch = body.clone();
    mismatch["github_login"] = json!("mallory");
    assert!(matches!(
        verify_offer(&devices, host_id, &mismatch, ts),
        Err(Refusal::BadSignature)
    ));
}

#[test]
fn invite_code_input_normalizes_hand_typing() {
    let (_dir, db) = test_db();
    let issued = issue(&db, "octocat", 7, NOW, 7).expect("issue");
    // Case, dashes and spaces ignored; the CNR prefix optional.
    let typed = issued.code.to_lowercase().replace('-', " ");
    assert_eq!(
        normalize_code_input(&typed).as_deref(),
        Some(issued.code.as_str())
    );
    let bare: String = issued.code.chars().filter(|c| *c != '-').collect();
    assert_eq!(
        normalize_code_input(&bare[3..]).as_deref(),
        Some(issued.code.as_str())
    );
    // Crockford confusables fold: I/L read as 1, O reads as 0.
    assert_eq!(
        normalize_code_input("cnr iiii oooo iiii oooo").as_deref(),
        Some("CNR-1111-0000-1111-0000")
    );
    // Anything else is not a code: a login, a short code, a bad symbol.
    for junk in ["octocat", "CNR-123", "CNR-ZZZZ-ZZZZ-ZZZZ-ZZZ!", ""] {
        assert_eq!(normalize_code_input(junk), None, "{junk:?} is not a code");
    }
}

#[test]
fn uninvite_revokes_by_code_or_login() {
    let (_dir, db) = test_db();
    let first = issue(&db, "octocat", 7, NOW, 7).expect("issue");
    issue(&db, "octocat", 7, NOW, 7).expect("issue");
    issue(&db, "mallory", 42, NOW, 7).expect("issue");
    // Revoking by code removes exactly that row, in any state; a second
    // revoke finds nothing left.
    let hash = code_hash(&first.code);
    assert!(db.revoke_invite(&hash).expect("revoke"));
    assert!(!db.revoke_invite(&hash).expect("revoke again"));
    // Revoking by login removes every live row for that login only, and
    // matches case-insensitively like the rest of the trust store.
    assert_eq!(
        db.revoke_invites_for_login("OctoCat", NOW + 60)
            .expect("revoke"),
        1
    );
    let rows = db.list_invites().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].login, "mallory");
    assert_eq!(
        db.revoke_invites_for_login("octocat", NOW + 60)
            .expect("revoke"),
        0,
        "nothing live is left to revoke"
    );
    // Used and expired rows are not live, so a login revoke leaves them.
    db.consume_invites_for_uid(42, NOW + 60).expect("consume");
    issue(&db, "old", 9, NOW, -30).expect("already-expired issue");
    assert_eq!(
        db.revoke_invites_for_login("mallory", NOW + 60)
            .expect("revoke"),
        0
    );
    assert_eq!(
        db.revoke_invites_for_login("old", NOW + 60)
            .expect("revoke"),
        0
    );
}

/// Audit evidence: the `canopy remote invite octocat` transcript (code
/// issued, `invites` lists it, the publish frame carries its hash). Run
/// with output capture off so the transcript prints.
#[test]
fn invite_cli_transcript() {
    let (_dir, db) = test_db();
    let issued = issue(&db, "octocat", 7, NOW, 7).expect("issue");
    let expires = chrono::DateTime::<chrono::Utc>::from_timestamp(issued.expires_at, 0)
        .map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| issued.expires_at.to_string());
    println!("$ canopy remote invite octocat");
    println!("Invite code for octocat: {}", issued.code);
    let link = guest_link(
        "https://remote.univerlab.org",
        1001,
        "76805a63a6980c18",
        &issued.code,
    )
    .expect("link");
    println!("Guest link: {link}");
    println!("Expires: {expires}");
    println!("$ canopy remote invites");
    for row in db.list_invites().expect("list") {
        println!("{}  expires {expires}  live", row.login);
    }
    let body = trust::guest_allow_body(&[7], &live(&db, NOW + 60));
    println!("$ guest_allow publish:");
    println!("{body}");
    assert_eq!(body["invites"][0]["uid"], json!(7));
}

#[test]
fn guest_link_carries_the_code_in_the_fragment() {
    let (_dir, db) = test_db();
    let issued = issue(&db, "octocat", 7, NOW, 7).expect("issue");
    let link = guest_link(
        "https://remote.univerlab.org/",
        1001,
        "76805a63a6980c18",
        &issued.code,
    )
    .expect("link");
    assert_eq!(
        link,
        format!(
            "https://remote.univerlab.org/g/1001/76805a63a6980c18#invite={}",
            issued.code
        )
    );
    // The fragment holds the canonical code: the web sends it as is, the
    // relay hashes exactly what it receives, and that hash is the stored one.
    let (_, fragment) = link.split_once("#invite=").expect("fragment");
    assert_eq!(
        normalize_code_input(fragment).as_deref(),
        Some(issued.code.as_str())
    );
    assert_eq!(
        code_hash(fragment),
        db.list_invites().expect("list")[0].code_hash
    );
    // A local relay keeps its scheme and port.
    assert_eq!(
        guest_link("http://127.0.0.1:8787", 1001, "76805a63a6980c18", "CNR-X").as_deref(),
        Some("http://127.0.0.1:8787/g/1001/76805a63a6980c18#invite=CNR-X")
    );
    // No link the web would refuse: not an http(s) origin, uid 0, or a
    // host id that is not 16 lowercase hex.
    for (relay, uid, host) in [
        ("", 1001, "76805a63a6980c18"),
        ("wss://remote.univerlab.org", 1001, "76805a63a6980c18"),
        ("https://", 1001, "76805a63a6980c18"),
        ("https://remote.univerlab.org", 0, "76805a63a6980c18"),
        ("https://remote.univerlab.org", 1001, "76805A63A6980C18"),
        ("https://remote.univerlab.org", 1001, "76805a63"),
    ] {
        assert_eq!(
            guest_link(relay, uid, host, "CNR-X"),
            None,
            "{relay} {uid} {host}"
        );
    }
}

#[test]
fn guest_link_for_names_why_there_is_no_link() {
    assert_eq!(
        guest_link_for("https://remote.univerlab.org", 1001, None, "CNR-X"),
        Err(NoGuestLink::NoHostKey)
    );
    assert_eq!(
        guest_link_for("", 1001, Some("76805a63a6980c18"), "CNR-X"),
        Err(NoGuestLink::BadRelayUrl)
    );
    assert!(NoGuestLink::NoHostKey.hint().contains("canopy remote on"));
    assert!(NoGuestLink::BadRelayUrl.hint().contains("relay_url"));
    assert_eq!(
        guest_link_for(
            "https://remote.univerlab.org",
            1001,
            Some("76805a63a6980c18"),
            "CNR-X"
        )
        .as_deref(),
        Ok("https://remote.univerlab.org/g/1001/76805a63a6980c18#invite=CNR-X")
    );
}
