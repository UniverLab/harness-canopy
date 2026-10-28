//! Vector and refusal tests for the signaling contract (Intelligence
//! d4eb8558).

use base64::Engine as _;
use serde_json::json;

use crate::db::Database;
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::{verify_p1363, HostKey};
use crate::remote::signaling::{
    answer_body, answer_message, connect_fail, offer_message, verify_offer, Refusal,
    MAX_CLOCK_SKEW_SECS,
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
