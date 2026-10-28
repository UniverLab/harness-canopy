//! The relay signaling contract: offers, answers and refusals.
//!
//! Every body carries `t`. An `offer` is accepted only for a paired device,
//! inside the 120 s clock window, and only when its ECDSA P-256 SHA-256
//! signature in IEEE P1363 form verifies over
//! `canopy-remote/v1/offer\n` + `host_id` + `\n` + `ts` + `\n` + `sdp` with
//! the device's stored public key. Signing the SDP pins the DTLS fingerprint,
//! so the data channel is end-to-end authenticated even when the relay and
//! TURN see the media path.
//!
//! This module is pure: it never touches the network and never builds a peer.

use base64::Engine as _;
use serde_json::{json, Value};

use crate::db::remote_devices::RemoteDevice;

use super::devices::RemoteDevices;
use super::hostkey::{verify_p1363, HostKey};

/// Prefix of the signed offer payload.
pub const OFFER_PREFIX: &str = "canopy-remote/v1/offer\n";
/// Prefix of the signed answer payload.
pub const ANSWER_PREFIX: &str = "canopy-remote/v1/answer\n";
/// Accepted distance between the device clock and the host clock.
pub const MAX_CLOCK_SKEW_SECS: i64 = 120;
/// Concurrent device peers a host serves.
pub const MAX_DEVICE_PEERS: usize = 4;

/// Why an offer was not answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No `device_id`, or one that is not in `remote_devices`.
    UnknownDevice,
    /// `|ts - now|` is over [`MAX_CLOCK_SKEW_SECS`].
    Stale,
    /// Any malformed field or a signature that does not verify.
    BadSignature,
    /// [`MAX_DEVICE_PEERS`] peers are already live.
    Busy,
}

impl Refusal {
    /// The `connect_fail.reason` word for this refusal.
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::UnknownDevice => "unknown_device",
            Refusal::Stale => "stale",
            Refusal::BadSignature => "bad_signature",
            Refusal::Busy => "busy",
        }
    }
}

/// `connect_fail{reason}`: the only answer a refused offer gets.
pub fn connect_fail(refusal: Refusal) -> Value {
    json!({"t": "connect_fail", "reason": refusal.reason()})
}

/// A verified offer, with the device row its signature was checked against.
#[derive(Debug, Clone)]
pub struct Offer {
    /// The paired device that sent it.
    pub device: RemoteDevice,
    /// Unix seconds from the signed body.
    #[allow(dead_code)]
    pub ts: i64,
    /// The SDP offer verbatim.
    pub sdp: String,
    /// Raw 64-byte P1363 signature.
    #[allow(dead_code)]
    pub sig: Vec<u8>,
    /// The signature exactly as the device sent it (base64), echoed in the
    /// answer's signed payload.
    pub sig_b64: String,
}

/// The exact bytes a device signs.
pub fn offer_message(host_id: &str, ts: i64, sdp: &str) -> String {
    format!("{OFFER_PREFIX}{host_id}\n{ts}\n{sdp}")
}

/// The exact bytes the host signs for an answer.
pub fn answer_message(device_id: &str, offer_sig_b64: &str, sdp: &str) -> String {
    format!("{ANSWER_PREFIX}{device_id}\n{offer_sig_b64}\n{sdp}")
}

/// Verify one `offer` body against the registry. The checks run in the order
/// the contract fixes: `unknown_device`, then `stale`, then `bad_signature`.
pub fn verify_offer(
    devices: &RemoteDevices,
    host_id: &str,
    body: &Value,
    now: i64,
) -> Result<Offer, Refusal> {
    let device_id = body
        .get("device_id")
        .and_then(Value::as_str)
        .ok_or(Refusal::UnknownDevice)?;
    let device = devices
        .get(device_id)
        .map_err(|_| Refusal::BadSignature)?
        .ok_or(Refusal::UnknownDevice)?;
    let ts = body
        .get("ts")
        .and_then(Value::as_i64)
        .ok_or(Refusal::BadSignature)?;
    if (ts - now).abs() > MAX_CLOCK_SKEW_SECS {
        return Err(Refusal::Stale);
    }
    let sdp = body
        .get("sdp")
        .and_then(Value::as_str)
        .ok_or(Refusal::BadSignature)?;
    let sig_b64 = body
        .get("sig")
        .and_then(Value::as_str)
        .ok_or(Refusal::BadSignature)?;
    let sig = decode_b64(sig_b64).ok_or(Refusal::BadSignature)?;
    if sig.len() != 64 {
        return Err(Refusal::BadSignature);
    }
    let message = offer_message(host_id, ts, sdp);
    if !verify_p1363(&device.spki, message.as_bytes(), &sig) {
        return Err(Refusal::BadSignature);
    }
    Ok(Offer {
        device,
        ts,
        sdp: sdp.to_string(),
        sig,
        sig_b64: sig_b64.to_string(),
    })
}

/// `answer{sdp,sig}`: sign the answer SDP bound to the offer signature.
pub fn answer_body(host: &HostKey, device_id: &str, offer_sig_b64: &str, sdp: &str) -> Value {
    let message = answer_message(device_id, offer_sig_b64, sdp);
    let sig = host.sign(message.as_bytes());
    json!({"t": "answer", "sdp": sdp, "sig": encode_b64(&sig)})
}

/// `ice{candidate}`: one trickled local candidate.
pub fn ice_body(candidate: &str) -> Value {
    json!({"t": "ice", "candidate": candidate})
}

/// The candidate string of an inbound `ice` body, if it carries one.
pub fn ice_candidate(body: &Value) -> Option<&str> {
    body.get("candidate").and_then(Value::as_str)
}

fn encode_b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_b64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

#[cfg(test)]
#[path = "signaling_tests.rs"]
mod tests;
