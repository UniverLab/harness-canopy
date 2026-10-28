use super::*;
use crate::db::Database;
use serde_json::json;

const DEVICE_ID: &str = "bfcba2b742a807bb";

fn test_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    std::mem::forget(dir);
    db
}

fn device_key() -> HostKey {
    HostKey::from_scalar_bytes(&[0x22; 32]).expect("device scalar")
}

fn host_key() -> HostKey {
    HostKey::from_scalar_bytes(&[0x11; 32]).expect("host scalar")
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn vector_code_state(path_dir: &std::path::Path) -> PairingState {
    let state = PairingState::new(path_dir);
    let normalized = "7K3QHM9P2X";
    state
        .write(&StoredCode {
            normalized: normalized.to_string(),
            k_pair: b64(&derive_k_pair(normalized, "76805a63a6980c18")),
            created_at: 1_000,
            fails: 0,
        })
        .expect("install vector code");
    state
}

#[test]
fn d4eb8558_k_pair_vector() {
    let key = derive_k_pair("7K3QHM9P2X", "76805a63a6980c18");
    assert_eq!(
        key.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "4ef92258602a96db9c8331090f7fdcfcda87d47480d5e0efc0ac2e80cfdb74c0"
    );
}

#[test]
fn d4eb8558_mac_vectors() {
    let key = derive_k_pair("7K3QHM9P2X", "76805a63a6980c18");
    let device = device_key();
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let nonce_h = b64(&(16u8..32).collect::<Vec<_>>());
    let init = mac_init(&key, &device.spki_b64(), &nonce_b);
    assert_eq!(b64(&init), "kpAbG/Gfg4QfoIVR/ey/Iop5TcNowq4mIrO29NyTL6o=");
    let ok = mac_ok(
        &key,
        &host_key().spki_b64(),
        &device.spki_b64(),
        &nonce_b,
        &nonce_h,
    );
    assert_eq!(b64(&ok), "1dCqDScWc5KVefuLcEqLHPTPVfNvO1QsjcW/kytm/RE=");
}

#[test]
fn d4eb8558_full_exchange_returns_the_vector_reply_and_stores_the_device() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let devices = RemoteDevices::new(db);
    let state = vector_code_state(dir.path());
    let device = device_key();
    let nonce_b = (0u8..16).collect::<Vec<_>>();
    let nonce_h: [u8; 16] = (16u8..32).collect::<Vec<_>>().try_into().expect("16 bytes");
    let key = derive_k_pair("7K3QHM9P2X", "76805a63a6980c18");
    let body = json!({
        "t": "pair_init",
        "device_spki": device.spki_b64(),
        "device_label": "laptop",
        "nonce_b": b64(&nonce_b),
        "mac": b64(&mac_init(&key, &device.spki_b64(), &b64(&nonce_b))),
    });
    let reply = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_001, nonce_h)
        .expect("respond");
    assert_eq!(reply["t"], "pair_ok");
    assert_eq!(reply["host_spki"], host_key().spki_b64());
    assert_eq!(reply["nonce_h"], b64(&nonce_h));
    assert_eq!(reply["mac"], "1dCqDScWc5KVefuLcEqLHPTPVfNvO1QsjcW/kytm/RE=");
    let stored = devices.list().expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, DEVICE_ID);
    assert_eq!(stored[0].label, "laptop");
    assert_eq!(stored[0].github_login, "octocat");
}

#[test]
fn code_is_single_use() {
    let dir = tempfile::tempdir().expect("tempdir");
    let devices = RemoteDevices::new(test_db());
    let state = vector_code_state(dir.path());
    let device = device_key();
    let key = derive_k_pair("7K3QHM9P2X", "76805a63a6980c18");
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let body = json!({
        "t": "pair_init",
        "device_spki": device.spki_b64(),
        "device_label": "laptop",
        "nonce_b": nonce_b,
        "mac": b64(&mac_init(&key, &device.spki_b64(), &nonce_b)),
    });
    let first = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_001, [0u8; 16])
        .expect("first");
    assert_eq!(first["t"], "pair_ok");
    let second = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_002, [0u8; 16])
        .expect("second");
    assert_eq!(second["t"], "pair_fail");
    assert_eq!(second["reason"], "no_code");
}

#[test]
fn expired_code_reports_expired_then_no_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let devices = RemoteDevices::new(test_db());
    let state = vector_code_state(dir.path());
    let device = device_key();
    let key = derive_k_pair("7K3QHM9P2X", "76805a63a6980c18");
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let body = json!({
        "t": "pair_init",
        "device_spki": device.spki_b64(),
        "device_label": "laptop",
        "nonce_b": nonce_b,
        "mac": b64(&mac_init(&key, &device.spki_b64(), &nonce_b)),
    });
    let expired = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_601, [0u8; 16])
        .expect("expired");
    assert_eq!(expired["reason"], "expired");
    assert!(devices.list().expect("list").is_empty());
    let gone = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_602, [0u8; 16])
        .expect("gone");
    assert_eq!(gone["reason"], "no_code");
}

#[test]
fn five_failed_macs_burn_the_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let devices = RemoteDevices::new(test_db());
    let state = vector_code_state(dir.path());
    let body = json!({
        "t": "pair_init",
        "device_spki": device_key().spki_b64(),
        "device_label": "laptop",
        "nonce_b": b64(&[0u8; 16]),
        "mac": b64(&[0u8; 32]),
    });
    for attempt in 1..=MAX_FAILURES {
        let reply = state
            .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_001, [0u8; 16])
            .expect("respond");
        assert_eq!(reply["reason"], "bad_code", "attempt {attempt}");
    }
    let after = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 1_001, [0u8; 16])
        .expect("after burn");
    assert_eq!(after["reason"], "no_code");
    assert!(devices.list().expect("list").is_empty());
}

#[test]
fn normalization_maps_ambiguous_letters_and_separators() {
    assert_eq!(normalize_code("7k3qh-m9p2x"), "7K3QHM9P2X");
    assert_eq!(normalize_code("7K3QH M9P2X"), "7K3QHM9P2X");
    assert_eq!(normalize_code("ilILoO01"), "11110001");
    assert_eq!(normalize_code("  a-b\tc d "), "ABCD");
}

#[test]
fn generated_codes_are_crockford_and_formatted() {
    let code = generate_code();
    assert_eq!(code.len(), CODE_LEN);
    assert!(code.bytes().all(|byte| ALPHABET.contains(&byte)));
    let formatted = format_code(&code);
    assert_eq!(formatted.len(), CODE_LEN + 1);
    assert_eq!(formatted.as_bytes()[5], b'-');
    assert_eq!(normalize_code(&formatted), code);
}

#[test]
fn creating_a_new_code_replaces_the_old_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let devices = RemoteDevices::new(test_db());
    let state = PairingState::new(dir.path());
    let first = state.create("76805a63a6980c18", 100).expect("first");
    let second = state.create("76805a63a6980c18", 200).expect("second");
    assert_ne!(first.code, second.code);
    let device = device_key();
    let key = derive_k_pair(&normalize_code(&second.code), "76805a63a6980c18");
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let body = json!({
        "t": "pair_init",
        "device_spki": device.spki_b64(),
        "device_label": "laptop",
        "nonce_b": nonce_b,
        "mac": b64(&mac_init(&key, &device.spki_b64(), &nonce_b)),
    });
    let reply = state
        .respond_with_nonce(&body, &host_key(), &devices, "octocat", 201, [0u8; 16])
        .expect("respond");
    assert_eq!(reply["t"], "pair_ok");
}
