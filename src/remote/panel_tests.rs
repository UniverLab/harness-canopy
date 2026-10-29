use super::*;
use crate::application::ports::StateRepository as _;
use crate::remote::auth::{self, GitHubIdentity};
use crate::remote::devices::{device_id, RemoteDevices};
use crate::remote::hostkey::HostKey;

fn test_db(dir: &tempfile::TempDir) -> Database {
    Database::new(&dir.path().join("test.db")).expect("database")
}

fn signed_in_identity() -> GitHubIdentity {
    GitHubIdentity {
        token: "[REDACTED]".to_string(),
        login: "octocat".to_string(),
        id: 1,
    }
}

#[test]
fn status_view_signed_out_reports_identity_and_switch_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db(&dir);
    let live = RemoteLive::new();

    let view = status_view(dir.path(), &db, &live).expect("status");
    assert!(!view.signed_in);
    assert_eq!(view.login, None);
    assert!(!view.enabled, "remote is off by default");
    assert_eq!(view.relay_url, "https://remote.univerlab.org");
    assert_eq!(view.relay, "off");
    assert!(view.live_devices.is_empty());
    assert!(view.devices.is_empty());

    auth::save_identity(dir.path(), &signed_in_identity()).expect("identity");
    set_enabled(dir.path(), true).expect("on");
    let view = status_view(dir.path(), &db, &live).expect("status");
    assert!(view.signed_in);
    assert_eq!(view.login.as_deref(), Some("octocat"));
    assert!(view.enabled);
}

#[test]
fn status_view_spells_each_relay_phase() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db(&dir);
    let live = RemoteLive::new();
    for (phase, text) in [
        (RelayPhase::Off, "off"),
        (RelayPhase::Connecting, "connecting"),
        (RelayPhase::Connected, "connected"),
        (RelayPhase::TokenRejected, "token rejected: sign in again"),
    ] {
        live.set_phase(phase);
        let view = status_view(dir.path(), &db, &live).expect("status");
        assert_eq!(view.relay, text, "phase {phase:?}");
        assert_eq!(relay_text(phase), text);
    }
}

#[test]
fn status_view_marks_live_devices() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db(&dir);
    let devices = RemoteDevices::new(db.clone());
    let live = RemoteLive::new();

    let spki = HostKey::from_scalar_bytes(&[0x22; 32])
        .expect("device")
        .spki_der()
        .to_vec();
    let id = device_id(&spki);
    devices
        .pair(spki, "laptop", "octocat", "2026-01-01T00:00:00Z")
        .expect("pair");
    let (control, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(live.peers().register(
        &id,
        "d:1",
        std::sync::Arc::new(tokio_util::sync::CancellationToken::new()),
        control
    ));

    let view = status_view(dir.path(), &db, &live).expect("status");
    assert_eq!(view.live_devices, vec![id.clone()]);
    assert_eq!(view.devices.len(), 1);
    assert_eq!(view.devices[0].id, id);
    assert_eq!(view.devices[0].label, "laptop");
    assert!(view.devices[0].live);

    live.peers().close(&id);
    let view = status_view(dir.path(), &db, &live).expect("status");
    assert!(view.live_devices.is_empty());
    assert!(!view.devices[0].live, "closing the peer clears the flag");
}

#[test]
fn create_pairing_returns_code_link_qr_and_expiry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let before = chrono::Utc::now().timestamp();
    let pair = create_pairing(dir.path()).expect("pair");

    assert_eq!(pair.code.len(), 11, "XXXXX-XXXXX: {}", pair.code);
    assert_eq!(&pair.code[5..6], "-");
    assert!(
        pair.link.starts_with("https://remote.univerlab.org/pair#"),
        "link: {}",
        pair.link
    );
    assert!(pair.link.ends_with(&pair.code), "link: {}", pair.link);
    assert!(
        (before + 599..=before + 601).contains(&pair.expires_at),
        "ten minutes out: {}",
        pair.expires_at
    );

    let lines: Vec<&str> = pair.qr.lines().collect();
    assert_eq!(lines.len(), 17, "level L fits the overlay: {}", pair.qr);
    assert!(
        lines.iter().all(|line| line.chars().count() == 33),
        "33 wide: {}",
        pair.qr
    );
}

#[test]
fn set_enabled_flips_the_switch_both_ways() {
    use crate::domain::canopy_config::CanopyConfig;

    let dir = tempfile::tempdir().expect("tempdir");
    set_enabled(dir.path(), true).expect("on");
    assert!(CanopyConfig::load(dir.path()).remote.enabled);
    set_enabled(dir.path(), false).expect("off");
    assert!(!CanopyConfig::load(dir.path()).remote.enabled);
}

#[test]
fn revoke_deletes_the_row_and_queues_the_id() {
    use crate::remote::relay::REMOTE_REVOKE_KEY;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db(&dir);
    let devices = RemoteDevices::new(db.clone());
    let spki = b"device spki bytes".to_vec();
    let id = device_id(&spki);
    devices
        .pair(spki, "laptop", "octocat", "2026-01-01T00:00:00Z")
        .expect("pair");

    assert!(
        !revoke(&db, "missingdevice0000").expect("unknown"),
        "unknown id revokes nothing"
    );
    assert_eq!(
        db.get_state(REMOTE_REVOKE_KEY).expect("state"),
        None,
        "an unknown id queues nothing"
    );

    assert!(revoke(&db, &id).expect("revoke"));
    assert!(devices.list().expect("list").is_empty());
    assert_eq!(
        db.get_state(REMOTE_REVOKE_KEY)
            .expect("state")
            .unwrap_or_default(),
        id,
        "the daemon picks the id up on its next tick"
    );
}

#[test]
fn disconnect_queues_the_id_and_keeps_the_row() {
    use crate::remote::relay::REMOTE_DISCONNECT_KEY;

    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db(&dir);
    let devices = RemoteDevices::new(db.clone());
    let spki = b"device spki bytes".to_vec();
    let id = device_id(&spki);
    devices
        .pair(spki, "laptop", "octocat", "2026-01-01T00:00:00Z")
        .expect("pair");

    assert!(
        !disconnect(&db, "missingdevice0000").expect("unknown"),
        "unknown id disconnects nothing"
    );
    assert_eq!(
        db.get_state(REMOTE_DISCONNECT_KEY).expect("state"),
        None,
        "an unknown id queues nothing"
    );

    assert!(disconnect(&db, &id).expect("disconnect"));
    assert_eq!(devices.list().expect("list").len(), 1, "the pairing stays");
    assert_eq!(
        db.get_state(REMOTE_DISCONNECT_KEY)
            .expect("state")
            .unwrap_or_default(),
        id,
        "the daemon picks the id up on its next tick"
    );
}
