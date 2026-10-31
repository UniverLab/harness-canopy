//! The device registry as the remote layer sees it.
//!
//! Every read and write goes through [`crate::db::Database`]; this wrapper
//! exists so the relay and CLI share one `is_paired` gate and so the next
//! spec's live connections all check the same function.

use anyhow::Result;
use chrono::Utc;
use sha2::{Digest as _, Sha256};

use crate::db::remote_devices::RemoteDevice;
use crate::db::Database;

/// Format an RFC 3339 UTC timestamp, the only format the registry stores.
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// `device_id` = first 16 lowercase hex chars of SHA-256(SPKI DER).
pub fn device_id(spki: &[u8]) -> String {
    let digest = Sha256::digest(spki);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Handle on the `remote_devices` table.
#[derive(Clone)]
pub struct RemoteDevices {
    db: Database,
}

impl RemoteDevices {
    /// Wrap a database handle.
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Store a newly paired device, or refresh label and `paired_at` when
    /// the same key pairs again. An empty label becomes `browser`.
    pub fn pair(
        &self,
        spki: Vec<u8>,
        label: &str,
        github_login: &str,
        paired_at: &str,
    ) -> Result<RemoteDevice> {
        let device = RemoteDevice {
            id: device_id(&spki),
            spki,
            label: if label.trim().is_empty() {
                "browser".to_string()
            } else {
                label.trim().to_string()
            },
            github_login: github_login.to_string(),
            paired_at: paired_at.to_string(),
            last_seen: None,
        };
        self.db.upsert_remote_device(&device)?;
        Ok(device)
    }

    /// Every paired device, oldest pairing first.
    pub fn list(&self) -> Result<Vec<RemoteDevice>> {
        self.db.list_remote_devices()
    }

    /// Whether this device id is currently paired.
    #[allow(dead_code)]
    pub fn is_paired(&self, id: &str) -> Result<bool> {
        self.db.is_remote_device_paired(id)
    }

    /// One paired device, or `None` when it was revoked.
    pub fn get(&self, id: &str) -> Result<Option<RemoteDevice>> {
        self.db.get_remote_device(id)
    }

    /// Mark a device as seen now (live connections).
    pub fn touch(&self, id: &str) -> Result<()> {
        self.db.touch_remote_device(id, &now_rfc3339())
    }

    /// Delete a device. Returns whether a row was removed.
    pub fn revoke(&self, id: &str) -> Result<bool> {
        self.db.delete_remote_device(id)
    }

    /// Whether a login holds a trust row. Fail closed: an unreadable store
    /// answers `false`, so guests never pair while the database is down.
    pub fn is_trusted_login(&self, login: &str) -> bool {
        self.db.get_trust(login).ok().flatten().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_the_first_16_hex_chars_of_sha256() {
        let der = b"not a real spki, only the hash matters here";
        let expected: String = Sha256::digest(der)[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(device_id(der), expected);
        assert_eq!(device_id(der).len(), 16);
        assert!(device_id(der).chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn re_pairing_the_same_key_updates_label_and_paired_at() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("test.db")).expect("database");
        let devices = RemoteDevices::new(db);
        let spki = b"device spki bytes".to_vec();
        let id = device_id(&spki);
        assert!(!devices.is_paired(&id).expect("unpaired at first"));
        devices
            .pair(spki.clone(), "laptop", "octocat", "2026-01-01T00:00:00Z")
            .expect("pair");
        assert!(devices.is_paired(&id).expect("paired"));
        devices
            .pair(spki, "renamed", "octocat", "2026-02-01T00:00:00Z")
            .expect("re-pair");
        let rows = devices.list().expect("list");
        assert_eq!(rows.len(), 1, "re-pairing must not add a row");
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].label, "renamed");
        assert_eq!(rows[0].paired_at, "2026-02-01T00:00:00Z");
    }
}
