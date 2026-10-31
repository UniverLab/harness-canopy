//! `remote_devices` rows: browsers paired with this host.
//!
//! A device is identified by the first 16 hex chars of SHA-256 over its
//! P-256 SPKI DER. Re-pairing the same key updates the label and
//! `paired_at`; revoking deletes the row, which is what the next spec's
//! live connections check through [`crate::remote::devices::RemoteDevices::is_paired`].

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Database;

/// The `remote_devices` table, shared by the base schema and the migration.
pub(super) const REMOTE_DEVICES_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS remote_devices (
                id TEXT PRIMARY KEY,
                spki BLOB NOT NULL,
                label TEXT NOT NULL,
                github_login TEXT NOT NULL,
                paired_at TEXT NOT NULL,
                last_seen TEXT
            );";

/// One paired browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDevice {
    /// First 16 hex chars of SHA-256(SPKI DER).
    pub id: String,
    /// Raw SPKI DER, exactly as the browser sent it.
    pub spki: Vec<u8>,
    /// Human label supplied by the browser.
    pub label: String,
    /// GitHub login the host was signed in as when the device paired.
    pub github_login: String,
    /// RFC 3339 UTC.
    pub paired_at: String,
    /// RFC 3339 UTC, `None` until the first connection (next spec).
    pub last_seen: Option<String>,
}

impl Database {
    /// Insert a device, or update label and `paired_at` for a device whose
    /// key is already known.
    pub fn upsert_remote_device(&self, device: &RemoteDevice) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO remote_devices (id, spki, label, github_login, paired_at, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                spki = excluded.spki,
                label = excluded.label,
                github_login = excluded.github_login,
                paired_at = excluded.paired_at,
                last_seen = excluded.paired_at",
            params![
                device.id,
                device.spki,
                device.label,
                device.github_login,
                device.paired_at,
                device.last_seen
            ],
        )?;
        Ok(())
    }

    /// Every paired device, oldest pairing first.
    pub fn list_remote_devices(&self) -> Result<Vec<RemoteDevice>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, spki, label, github_login, paired_at, last_seen
             FROM remote_devices ORDER BY paired_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RemoteDevice {
                id: row.get(0)?,
                spki: row.get(1)?,
                label: row.get(2)?,
                github_login: row.get(3)?,
                paired_at: row.get(4)?,
                last_seen: row.get(5)?,
            })
        })?;
        let mut devices = Vec::new();
        for row in rows {
            devices.push(row?);
        }
        Ok(devices)
    }

    /// Delete one device. Returns whether a row was removed.
    pub fn delete_remote_device(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let removed = conn.execute("DELETE FROM remote_devices WHERE id = ?1", [id])?;
        Ok(removed > 0)
    }

    /// Whether a device id is still paired.
    #[allow(dead_code)]
    pub fn is_remote_device_paired(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let found: Option<i64> = conn
            .query_row("SELECT 1 FROM remote_devices WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(found.is_some())
    }

    /// One paired device by id, with the public key its signatures verify
    /// against.
    pub fn get_remote_device(&self, id: &str) -> Result<Option<RemoteDevice>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let device = conn
            .query_row(
                "SELECT id, spki, label, github_login, paired_at, last_seen
                 FROM remote_devices WHERE id = ?1",
                [id],
                |row| {
                    Ok(RemoteDevice {
                        id: row.get(0)?,
                        spki: row.get(1)?,
                        label: row.get(2)?,
                        github_login: row.get(3)?,
                        paired_at: row.get(4)?,
                        last_seen: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(device)
    }

    /// Record the last time a paired device was seen.
    pub fn touch_remote_device(&self, id: &str, seen_at: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE remote_devices SET last_seen = ?1 WHERE id = ?2",
            params![seen_at, id],
        )?;
        Ok(())
    }
}
