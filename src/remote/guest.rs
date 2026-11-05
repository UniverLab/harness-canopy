//! Guest connections: relay-attested identity without a device row.
//!
//! A guest arrives through the invite door, not pairing, so it owns no
//! device row and signs nothing. The relay announces each guest connection
//! to the hosts as its own notice, `{"from":"relay","body":{"t":
//! "device_identity","conn":<id>,"role":"guest","login":…,"uid":…}}`, and
//! the host tracks that connection as `d:<id>`, the `from` its frames carry;
//! `device_left` forgets it. A notice from any other sender is ignored: a
//! device or guest could otherwise announce itself under any login.
//!
//! The relay forwards bodies verbatim but sends every device or guest frame
//! to a host as `{"from":"d:<id>","login":…,"uid":…,"body":…}`, the
//! `login`/`uid` siblings written from its own connection table:
//! [`attested_body`] copies those two over the body's own (dropping any the
//! relay did not attest), so a guest offer verifies against them and the
//! tracked identity. Anything else identity-shaped in the body
//! (`device_id`, `github_login`, `sig`) is client-writable and is ignored
//! on the guest path.

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
    /// Attested uid, equal to the tracked one.
    pub uid: u64,
    /// The SDP offer verbatim.
    pub sdp: String,
    /// What this guest's login may see.
    pub grant: Grant,
    /// Whether this guest holds the `upload` grant.
    pub upload: bool,
}

/// The `from` of the relay's own notices (`device_identity`, `device_left`).
pub const RELAY_FROM: &str = "relay";

/// The connection a relay notice names, as its frames' `from` (`d:<conn>`);
/// `None` unless the relay itself sent the notice and it names one.
fn notice_conn(body: &Value, from: &str) -> Option<String> {
    if from != RELAY_FROM {
        return None;
    }
    let conn = body.get("conn").and_then(Value::as_str)?;
    if conn.is_empty() || conn.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("d:{conn}"))
}

/// One relay envelope's body with the relay's attestation applied. For a
/// device or guest frame (`from: "d:…"`) the envelope's top-level `login`
/// and `uid`, written by the relay from its connection table, replace the
/// body's own, and a body `login`/`uid` the relay did not attest is
/// dropped, so client bytes never count as identity. Any other envelope's
/// body (the relay's own notices) is returned as is. `None` without a body.
pub fn attested_body(envelope: &Value) -> Option<Value> {
    let mut body = envelope.get("body")?.clone();
    let from = envelope.get("from").and_then(Value::as_str).unwrap_or("");
    if !from.starts_with("d:") {
        return Some(body);
    }
    if let Some(fields) = body.as_object_mut() {
        for key in ["login", "uid"] {
            match envelope.get(key) {
                Some(value) if !value.is_null() => {
                    fields.insert(key.to_string(), value.clone());
                }
                _ => {
                    fields.remove(key);
                }
            }
        }
    }
    Some(body)
}

/// Relay `from` ids currently known to be guest connections.
#[derive(Debug, Default)]
pub struct GuestConns {
    conns: Mutex<HashMap<String, GuestIdentity>>,
}

impl GuestConns {
    /// Track one `device_identity` notice from the relay (`from` is the
    /// envelope's): only `role: "guest"` with a `conn`, a non-blank `login`
    /// and a numeric `uid` is kept, under `d:<conn>`; anything else (device
    /// notices, malformed bodies, a notice from any other sender) leaves
    /// the table unchanged.
    pub fn note_device_identity(&self, body: &Value, from: &str) {
        if body.get("role").and_then(Value::as_str) != Some("guest") {
            return;
        }
        let Some(conn) = notice_conn(body, from) else {
            return;
        };
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
                conn,
                GuestIdentity {
                    login: login.to_string(),
                    uid,
                },
            );
        }
    }

    /// Forget the connection one `device_left` notice from the relay names.
    pub fn note_device_left(&self, body: &Value, from: &str) {
        let Some(conn) = notice_conn(body, from) else {
            return;
        };
        if let Ok(mut conns) = self.conns.lock() {
            conns.remove(&conn);
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

    /// Verify one guest `offer` body (already through [`attested_body`]): the
    /// sender must be a tracked guest, the relay-attested `login`/`uid` must
    /// both be present and equal the tracked identity, and the attested login must hold a grant. Any
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
        let upload = trust::resolve_upload(db, owner_login, login);
        Ok(GuestOffer {
            uid,
            sdp: sdp.to_string(),
            grant,
            upload,
        })
    }
}
