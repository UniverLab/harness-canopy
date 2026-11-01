//! Guest connections: relay-attested identity without a device row.
//!
//! A guest arrives through the invite door, not pairing, so it owns no
//! device row and signs nothing. The host tracks guest `from` ids from the
//! relay's `device_identity` notices (`role: "guest"` plus the connection's
//! `login`/`uid`), and a guest offer verifies against that tracked identity:
//! the relay overwrites the body's top-level `login`/`uid` from its own
//! connection table before forwarding, so those two fields are the
//! attestation. Anything else identity-shaped in the body (`device_id`,
//! `github_login`, `sig`) is client-writable — the relay forwards bodies
//! verbatim — and is ignored on the guest path.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use super::signaling::Refusal;
use super::trust::{self, Grant};
use crate::db::Database;

/// One tracked guest connection: the login and uid the relay bound to it.
#[derive(Debug, Clone)]
pub struct GuestIdentity {
    /// GitHub login, as the relay spelled it.
    pub login: String,
    /// GitHub uid, as the relay reported it.
    pub uid: u64,
}

/// A verified guest offer: the SDP plus the identity the grant resolved for.
#[derive(Debug, Clone)]
pub struct GuestOffer {
    /// Injected uid, equal to the tracked one.
    pub uid: u64,
    /// The SDP offer verbatim.
    pub sdp: String,
    /// What this guest's login may see.
    pub grant: Grant,
}

/// Relay `from` ids currently known to be guest connections.
#[derive(Debug, Default)]
pub struct GuestConns {
    conns: Mutex<HashMap<String, GuestIdentity>>,
}

impl GuestConns {
    /// Track one `device_identity` notice: only `role: "guest"` with a
    /// non-blank `login` and a numeric `uid` is kept; anything else
    /// (device notices, malformed bodies) leaves the table unchanged.
    pub fn note_device_identity(&self, body: &Value, from: &str) {
        if body.get("role").and_then(Value::as_str) != Some("guest") {
            return;
        }
        let (Some(login), Some(uid)) = (
            body.get("login").and_then(Value::as_str),
            body.get("uid").and_then(Value::as_u64),
        ) else {
            return;
        };
        if login.trim().is_empty() {
            return;
        }
        if let Ok(mut conns) = self.conns.lock() {
            conns.insert(
                from.to_string(),
                GuestIdentity {
                    login: login.to_string(),
                    uid,
                },
            );
        }
    }

    /// Whether `from` is a tracked guest connection.
    pub fn is_guest(&self, from: &str) -> bool {
        self.conns
            .lock()
            .is_ok_and(|conns| conns.contains_key(from))
    }

    /// The tracked identity for `from`, if any.
    fn get(&self, from: &str) -> Option<GuestIdentity> {
        self.conns.lock().ok()?.get(from).cloned()
    }

    /// Verify one guest `offer` body: the sender must be a tracked guest, the
    /// relay-injected top-level `login`/`uid` must both be present and equal
    /// the tracked identity, and the injected login must hold a grant. Any
    /// client-written identity in the body is ignored, not compared. Reads
    /// `forbidden` unless the body is malformed past identity (a missing
    /// SDP), which reads `bad_signature` like the device path.
    pub fn verify_guest_offer(
        &self,
        db: &Database,
        owner_login: &str,
        body: &Value,
        from: &str,
    ) -> Result<GuestOffer, Refusal> {
        let tracked = self.get(from).ok_or(Refusal::Forbidden)?;
        let login = body
            .get("login")
            .and_then(Value::as_str)
            .ok_or(Refusal::Forbidden)?;
        let uid = body
            .get("uid")
            .and_then(Value::as_u64)
            .ok_or(Refusal::Forbidden)?;
        if uid != tracked.uid
            || trust::normalize_login(login) != trust::normalize_login(&tracked.login)
        {
            return Err(Refusal::Forbidden);
        }
        let sdp = body
            .get("sdp")
            .and_then(Value::as_str)
            .ok_or(Refusal::BadSignature)?;
        let grant = trust::resolve(db, owner_login, login).ok_or(Refusal::Forbidden)?;
        Ok(GuestOffer {
            uid,
            sdp: sdp.to_string(),
            grant,
        })
    }
}
