//! Vector and refusal tests for the signaling contract (Intelligence
//! d4eb8558).

use base64::Engine as _;
use serde_json::json;

use crate::db::Database;
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::{verify_p1363, HostKey};
use crate::remote::signaling::{
    answer_body, answer_message, connect_fail, ice_body, ice_candidate, offer_message,
    verify_offer, verify_offer_seen, IceCandidate, Refusal, SeenOffers, MAX_CLOCK_SKEW_SECS,
    SEEN_OFFER_CAP,
};

const VECTOR_TS: i64 = 1_793_100_000;
const VECTOR_OFFER_SDP: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";
const VECTOR_ANSWER_SDP: &str = "v=0\r\no=- 3 4 IN IP4 127.0.0.1\r\n";
const VECTOR_OFFER_SIG: &str =
    "IjIQ+FdCMvFYZfODE7OpflYnZlg0aWjc3GWfDBK9CLk5f8TWURvcsDpoNbvvh4Iaz/7MUmkEo6571C6asBI6UQ==";
const VECTOR_ANSWER_SIG: &str =
    "cj0zq+BTNi23U+rvtXTSwl080GqqqkLVV057yGTxdQ4NCb5zrQhsKlbBvncvBx7hfXHjfv9Ete03GD6tJiLFMg==";
const DEVICE_ID: &str = "bfcba2b742a807bb";
const HOST_ID: &str = "76805a63a6980c18";

/// Host key from the d4eb8558 vectors (scalar 0x11).
fn host() -> HostKey {
    HostKey::from_scalar_bytes(&[0x11; 32]).expect("host vector key")
}

/// Device key from the d4eb8558 vectors (scalar 0x22).
fn device_key() -> HostKey {
    HostKey::from_scalar_bytes(&[0x22; 32]).expect("device vector key")
}

fn registry() -> (tempfile::TempDir, RemoteDevices) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    let devices = RemoteDevices::new(db);
    devices
        .pair(
            device_key().spki_der().to_vec(),
            "browser",
            "octocat",
            "2026-01-01T00:00:00Z",
        )
        .expect("pair vector device");
    (dir, devices)
}

fn offer_body(ts: i64, sdp: &str, sig: &str) -> serde_json::Value {
    json!({"t": "offer", "device_id": DEVICE_ID, "ts": ts, "sdp": sdp, "sig": sig})
}

#[test]
fn offer_signature_vector_verifies() {
    let (_dir, devices) = registry();
    let offer = verify_offer(
        &devices,
        HOST_ID,
        &offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG),
        VECTOR_TS,
    )
    .expect("the d4eb8558 offer must verify");
    assert_eq!(offer.device.id, DEVICE_ID);
    assert_eq!(
        offer.device.id,
        crate::remote::devices::device_id(device_key().spki_der())
    );
    assert_eq!(offer.ts, VECTOR_TS);
    assert_eq!(offer.sdp, VECTOR_OFFER_SDP);
    assert_eq!(offer.sig_b64, VECTOR_OFFER_SIG);
    assert_eq!(
        offer_message(HOST_ID, VECTOR_TS, VECTOR_OFFER_SDP),
        format!("canopy-remote/v1/offer\n{HOST_ID}\n{VECTOR_TS}\n{VECTOR_OFFER_SDP}")
    );
}

#[test]
fn answer_signature_vector_verifies_with_host_spki() {
    let message = answer_message(DEVICE_ID, VECTOR_OFFER_SIG, VECTOR_ANSWER_SDP);
    let signature = base64::engine::general_purpose::STANDARD
        .decode(VECTOR_ANSWER_SIG)
        .expect("vector base64");
    assert_eq!(signature.len(), 64, "P1363, not DER");
    assert!(
        verify_p1363(host().spki_der(), message.as_bytes(), &signature),
        "the d4eb8558 answer must verify with host_spki"
    );
    // The host's own answer signs the same payload and verifies with its key.
    let body = answer_body(&host(), DEVICE_ID, VECTOR_OFFER_SIG, VECTOR_ANSWER_SDP);
    let signed = base64::engine::general_purpose::STANDARD
        .decode(body["sig"].as_str().expect("sig"))
        .expect("base64");
    assert!(verify_p1363(
        host().spki_der(),
        answer_message(DEVICE_ID, VECTOR_OFFER_SIG, VECTOR_ANSWER_SDP).as_bytes(),
        &signed
    ));
    assert_eq!(body["sdp"], VECTOR_ANSWER_SDP);
}

#[test]
fn stale_timestamp_is_refused_before_the_signature_is_checked() {
    let (_dir, devices) = registry();
    let stale = verify_offer(
        &devices,
        HOST_ID,
        &offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG),
        VECTOR_TS + MAX_CLOCK_SKEW_SECS + 1,
    );
    assert_eq!(stale.err(), Some(Refusal::Stale));
    // A stale body whose signature is also garbage still reads `stale`.
    let both = verify_offer(
        &devices,
        HOST_ID,
        &offer_body(VECTOR_TS, VECTOR_OFFER_SDP, "not-base64"),
        VECTOR_TS + MAX_CLOCK_SKEW_SECS + 1,
    );
    assert_eq!(both.err(), Some(Refusal::Stale));
    let fresh = verify_offer(
        &devices,
        HOST_ID,
        &offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG),
        VECTOR_TS + MAX_CLOCK_SKEW_SECS,
    );
    assert!(fresh.is_ok(), "the window is inclusive");
}

#[test]
fn unknown_or_missing_device_is_refused() {
    let (_dir, empty) = {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("test.db")).expect("database");
        (dir, RemoteDevices::new(db))
    };
    let unknown = verify_offer(
        &empty,
        HOST_ID,
        &offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG),
        VECTOR_TS,
    );
    assert_eq!(unknown.err(), Some(Refusal::UnknownDevice));
    let mut missing = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, "garbage");
    missing.as_object_mut().expect("object").remove("device_id");
    assert_eq!(
        verify_offer(&empty, HOST_ID, &missing, VECTOR_TS).err(),
        Some(Refusal::UnknownDevice),
        "a missing device id is unknown_device, not bad_signature"
    );
}

#[test]
fn a_string_timestamp_verifies_like_the_contract_sends_it() {
    // The contract (d4eb8558) spells `ts` as a unix-seconds string; a browser
    // sends `{"ts": "1793100000"}`, so the verifier must accept it and sign
    // the same bytes as the number form.
    let (_dir, devices) = registry();
    let mut body = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG);
    body["ts"] = json!(VECTOR_TS.to_string());
    let offer = verify_offer(&devices, HOST_ID, &body, VECTOR_TS)
        .expect("a string ts is the contract's spelling");
    assert_eq!(offer.ts, VECTOR_TS);
    assert_eq!(offer.sdp, VECTOR_OFFER_SDP);
    assert_eq!(offer.sig_b64, VECTOR_OFFER_SIG);
    // A string ts is still refused outside the window.
    assert_eq!(
        verify_offer(
            &devices,
            HOST_ID,
            &body,
            VECTOR_TS + MAX_CLOCK_SKEW_SECS + 1
        )
        .err(),
        Some(Refusal::Stale)
    );
}

#[test]
fn a_tampered_sdp_or_signature_is_refused() {
    let (_dir, devices) = registry();
    let tampered = verify_offer(
        &devices,
        HOST_ID,
        &offer_body(
            VECTOR_TS,
            "v=0\r\no=- 9 9 IN IP4 10.0.0.1\r\n",
            VECTOR_OFFER_SIG,
        ),
        VECTOR_TS,
    );
    assert_eq!(tampered.err(), Some(Refusal::BadSignature));

    let mut bad_sig = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG);
    bad_sig["sig"] = json!("AAAA");
    assert_eq!(
        verify_offer(&devices, HOST_ID, &bad_sig, VECTOR_TS).err(),
        Some(Refusal::BadSignature)
    );

    // A DER-encoded signature of the right payload is 70-ish bytes and must
    // be refused: the wire format is P1363 only.
    use p256::ecdsa::signature::Signer as _;
    use p256::ecdsa::Signature;
    let signature: Signature = device_key()
        .signing_key()
        .sign(offer_message(HOST_ID, VECTOR_TS, VECTOR_OFFER_SDP).as_bytes());
    let der = base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes());
    assert_ne!(der, VECTOR_OFFER_SIG);
    let der_body = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, &der);
    assert_eq!(
        verify_offer(&devices, HOST_ID, &der_body, VECTOR_TS).err(),
        Some(Refusal::BadSignature)
    );
}

#[test]
fn a_replayed_offer_is_refused_as_bad_signature() {
    let (_dir, devices) = registry();
    let mut seen = SeenOffers::new();
    assert!(seen.is_empty());
    let body = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG);
    verify_offer_seen(&devices, HOST_ID, &body, VECTOR_TS, &mut seen)
        .expect("the first offer is fresh");
    assert_eq!(seen.len(), 1);
    let replay = verify_offer_seen(&devices, HOST_ID, &body, VECTOR_TS, &mut seen);
    assert_eq!(replay.err(), Some(Refusal::BadSignature));
    assert_eq!(
        connect_fail(Refusal::BadSignature)["reason"],
        "bad_signature",
        "a replay keeps the bad_signature wire reason"
    );
}

#[test]
fn a_fresh_signature_from_the_same_device_is_not_a_replay() {
    // The registry keys by signature hash, never by device: a device that
    // re-signs (new ts/sdp) after its first offer is accepted. A
    // device-keyed registry would refuse this second offer.
    let (_dir, devices) = registry();
    let mut seen = SeenOffers::new();
    let first = offer_body(VECTOR_TS, VECTOR_OFFER_SDP, VECTOR_OFFER_SIG);
    verify_offer_seen(&devices, HOST_ID, &first, VECTOR_TS, &mut seen)
        .expect("the first offer is fresh");
    let ts = VECTOR_TS + 10;
    let sdp = "v=0\r\no=- 5 6 IN IP4 127.0.0.1\r\n";
    let message = offer_message(HOST_ID, ts, sdp);
    let sig =
        base64::engine::general_purpose::STANDARD.encode(device_key().sign(message.as_bytes()));
    let second = offer_body(ts, sdp, &sig);
    verify_offer_seen(&devices, HOST_ID, &second, VECTOR_TS, &mut seen)
        .expect("a fresh signature from the same device is not a replay");
    assert_eq!(seen.len(), 2);
}

#[test]
fn a_seen_signature_expires_after_twice_the_clock_window() {
    // The TTL boundary lives in `check_and_insert`: the clock window would
    // refuse a re-signed body past 120 s as `stale` long before the registry
    // entry expires, so the boundary is driven with a synthetic hash.
    let mut seen = SeenOffers::new();
    let hash = [0xABu8; 32];
    assert!(!seen.check_and_insert(hash, VECTOR_TS));
    assert!(
        seen.check_and_insert(hash, VECTOR_TS + 239),
        "a repeat inside the 240 s window is still a replay"
    );
    assert!(
        !seen.check_and_insert(hash, VECTOR_TS + 240),
        "a retry once the entry has expired is fresh again"
    );
    assert_eq!(seen.len(), 1);
}

#[test]
fn the_seen_registry_stays_bounded_under_a_burst() {
    let mut seen = SeenOffers::new();
    for index in 0..1500u32 {
        let mut hash = [0u8; 32];
        hash[..4].copy_from_slice(&index.to_be_bytes());
        seen.check_and_insert(hash, VECTOR_TS);
    }
    assert!(
        seen.len() <= SEEN_OFFER_CAP,
        "1500 offers must not grow the registry past {SEEN_OFFER_CAP}: {}",
        seen.len()
    );
    // Distinct signatures never collide: two fresh hashes are both unseen,
    // so concurrent devices do not block each other.
    assert!(!seen.check_and_insert([0xAAu8; 32], VECTOR_TS));
    assert!(!seen.check_and_insert([0xBBu8; 32], VECTOR_TS));
    assert!(seen.len() <= SEEN_OFFER_CAP);
}

#[test]
fn refusal_reasons_are_the_contract_words() {
    assert_eq!(
        connect_fail(Refusal::UnknownDevice)["reason"],
        "unknown_device"
    );
    assert_eq!(connect_fail(Refusal::Stale)["reason"], "stale");
    assert_eq!(
        connect_fail(Refusal::BadSignature)["reason"],
        "bad_signature"
    );
    assert_eq!(connect_fail(Refusal::Busy)["reason"], "busy");
    assert_eq!(connect_fail(Refusal::Busy)["t"], "connect_fail");
}

// The trickle ICE contract vectors, quoted verbatim on both sides (host and
// web): the object form (`RTCIceCandidateInit`) is canonical, a legacy bare
// string still reads, and anything else is ignored.

/// The candidate-attribute string every vector below carries.
const VECTOR_CANDIDATE: &str = "candidate:1 1 UDP 1694498815 203.0.113.7 5000 typ host";

/// V1 `{"t":"ice","candidate":{"candidate":"C","sdpMid":"0","sdpMLineIndex":0}}`
/// parses with the candidate and both mids.
#[test]
fn ice_object_full_keeps_the_candidate_and_both_mids() {
    let body = json!({
        "t": "ice",
        "candidate": {
            "candidate": VECTOR_CANDIDATE,
            "sdpMid": "0",
            "sdpMLineIndex": 0,
        },
    });
    let parsed = ice_candidate(&body).expect("V1 parses");
    assert_eq!(parsed.candidate, VECTOR_CANDIDATE);
    assert_eq!(parsed.sdp_mid.as_deref(), Some("0"));
    assert_eq!(parsed.sdp_mline_index, Some(0));
}

/// V2 `{"t":"ice","candidate":{"candidate":"C"}}` parses with no mids.
#[test]
fn ice_object_minimal_parses_with_no_mids() {
    let body = json!({
        "t": "ice",
        "candidate": {"candidate": VECTOR_CANDIDATE},
    });
    let parsed = ice_candidate(&body).expect("V2 parses");
    assert_eq!(parsed.candidate, VECTOR_CANDIDATE);
    assert_eq!(parsed.sdp_mid, None);
    assert_eq!(parsed.sdp_mline_index, None);
}

/// V3 `{"t":"ice","candidate":"C"}` parses as a legacy bare string.
#[test]
fn ice_bare_string_parses_with_no_mids() {
    let body = json!({"t": "ice", "candidate": VECTOR_CANDIDATE});
    let parsed = ice_candidate(&body).expect("V3 parses");
    assert_eq!(parsed.candidate, VECTOR_CANDIDATE);
    assert_eq!(parsed.sdp_mid, None);
    assert_eq!(parsed.sdp_mline_index, None);
}

/// V4 `{"t":"ice"}` carries no candidate and is ignored.
#[test]
fn ice_missing_candidate_is_ignored() {
    assert_eq!(ice_candidate(&json!({"t": "ice"})), None);
}

/// V5 `{"t":"ice","candidate":null}` carries no candidate and is ignored.
#[test]
fn ice_null_candidate_is_ignored() {
    assert_eq!(
        ice_candidate(&json!({"t": "ice", "candidate": serde_json::Value::Null})),
        None
    );
}

/// The host emits the object form: a full candidate keeps all three keys, a
/// bare one emits the minimal object with no mid keys at all.
#[test]
fn ice_body_emits_the_object_form() {
    let full = IceCandidate {
        candidate: VECTOR_CANDIDATE.to_string(),
        sdp_mid: Some("0".to_string()),
        sdp_mline_index: Some(0),
    };
    let body = ice_body(&full);
    assert_eq!(body["t"], "ice");
    assert_eq!(body["candidate"]["candidate"], VECTOR_CANDIDATE);
    assert_eq!(body["candidate"]["sdpMid"], "0");
    assert_eq!(body["candidate"]["sdpMLineIndex"], 0);
    let bare = ice_body(&IceCandidate::bare(VECTOR_CANDIDATE.to_string()));
    assert_eq!(bare["t"], "ice");
    assert_eq!(bare["candidate"]["candidate"], VECTOR_CANDIDATE);
    assert!(
        bare["candidate"].get("sdpMid").is_none(),
        "a bare candidate emits no sdpMid: {bare}"
    );
    assert!(
        bare["candidate"].get("sdpMLineIndex").is_none(),
        "a bare candidate emits no sdpMLineIndex: {bare}"
    );
}

/// A browser-shaped candidate crosses the whole trickle path: parsed from the
/// V1 body, it reaches a live peer connection through `add_remote_candidate`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trickle_round_trip_reaches_the_peer_connection() {
    let (offer_sdp, _device) =
        crate::remote::peer::Peer::offer(Vec::new(), crate::remote::peer::CHANNEL_LABEL)
            .await
            .expect("the device offer");
    let (_answer_sdp, host) = crate::remote::peer::Peer::answer_offer(Vec::new(), &offer_sdp)
        .await
        .expect("the host answer");
    let body = json!({
        "t": "ice",
        "candidate": {
            "candidate": VECTOR_CANDIDATE,
            "sdpMid": "0",
            "sdpMLineIndex": 0,
        },
    });
    let parsed = ice_candidate(&body).expect("V1 parses");
    host.add_remote_candidate(&parsed)
        .await
        .expect("the browser-shaped candidate reaches the peer");
}
