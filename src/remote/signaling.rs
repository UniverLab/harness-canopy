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

use std::collections::HashMap;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::db::remote_devices::RemoteDevice;

use super::devices::RemoteDevices;
use super::hostkey::{verify_p1363, HostKey};
use super::trust;

/// Prefix of the signed offer payload.
pub const OFFER_PREFIX: &str = "canopy-remote/v1/offer\n";
/// Prefix of the signed answer payload.
pub const ANSWER_PREFIX: &str = "canopy-remote/v1/answer\n";
/// Accepted distance between the device clock and the host clock.
pub const MAX_CLOCK_SKEW_SECS: i64 = 120;
/// Concurrent device peers a host serves.
pub const MAX_DEVICE_PEERS: usize = 4;
/// How long an accepted offer signature stays in the replay registry: twice
/// the clock window, so no live offer can be replayed after it expires.
pub const SEEN_OFFER_TTL_SECS: i64 = 2 * MAX_CLOCK_SKEW_SECS;
/// Upper bound of the replay registry; the oldest entry is evicted past it.
pub const SEEN_OFFER_CAP: usize = 1024;

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
    /// The device is paired but its login holds no grant.
    Forbidden,
}

impl Refusal {
    /// The `connect_fail.reason` word for this refusal.
    pub fn reason(&self) -> &'static str {
        match self {
            Refusal::UnknownDevice => "unknown_device",
            Refusal::Stale => "stale",
            Refusal::BadSignature => "bad_signature",
            Refusal::Busy => "busy",
            Refusal::Forbidden => "forbidden",
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

/// The exact bytes a device signs, for tests and vector checks.
#[cfg(test)]
pub fn offer_message(host_id: &str, ts: i64, sdp: &str) -> String {
    offer_message_text(host_id, &ts.to_string(), sdp)
}

/// [`offer_message`] with the timestamp verbatim, so a device that signs its
/// own spelling of `ts` verifies byte for byte.
fn offer_message_text(host_id: &str, ts: &str, sdp: &str) -> String {
    format!("{OFFER_PREFIX}{host_id}\n{ts}\n{sdp}")
}

/// The offer's timestamp: its signed text plus the value used for the clock
/// window. The contract sends `ts` as a unix-seconds string (`"1793100000"`);
/// a JSON number is accepted too, and both keep the spelling the device
/// signed.
fn offer_ts(body: &Value) -> Option<(i64, String)> {
    match body.get("ts")? {
        Value::String(text) => Some((text.parse().ok()?, text.clone())),
        Value::Number(number) => Some((number.as_i64()?, number.to_string())),
        _ => None,
    }
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
    let (ts, ts_text) = offer_ts(body).ok_or(Refusal::BadSignature)?;
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
    // The body's login is a client hint, not an attestation — the relay
    // forwards bodies verbatim. It must match the login bound at pair time,
    // or the offer is not this device's; the signature over the device key
    // below is the real authentication. Absent on old owner offers, which
    // skip the hint check.
    if let Some(claimed) = body.get("github_login").and_then(Value::as_str) {
        if trust::normalize_login(claimed) != trust::normalize_login(&device.github_login) {
            return Err(Refusal::BadSignature);
        }
    }
    let message = offer_message_text(host_id, &ts_text, sdp);
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

/// Signatures of recently accepted offers, keyed by signature hash only: two
/// concurrent devices never share a signature, so neither blocks the other.
#[derive(Debug, Default)]
pub struct SeenOffers {
    seen: HashMap<[u8; 32], i64>,
}

impl SeenOffers {
    /// An empty replay registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries currently held, including unexpired ones only after a check.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether the registry holds no entries.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Prune expired entries, then report whether `hash` was already seen.
    /// A fresh hash is recorded; past [`SEEN_OFFER_CAP`] the oldest entry is
    /// evicted first so the registry stays bounded.
    pub fn check_and_insert(&mut self, hash: [u8; 32], now: i64) -> bool {
        self.seen
            .retain(|_, seen_at| *seen_at + SEEN_OFFER_TTL_SECS > now);
        if self.seen.contains_key(&hash) {
            return true;
        }
        if self.seen.len() >= SEEN_OFFER_CAP {
            evict_oldest(&mut self.seen);
        }
        self.seen.insert(hash, now);
        false
    }
}

/// Drop the oldest entry of a full replay registry.
fn evict_oldest(seen: &mut HashMap<[u8; 32], i64>) {
    let oldest = seen
        .iter()
        .min_by_key(|(_, seen_at)| **seen_at)
        .map(|(hash, _)| *hash);
    if let Some(hash) = oldest {
        seen.remove(&hash);
    }
}

/// The replay-registry key for one offer signature.
fn offer_sig_hash(sig: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(sig);
    hasher.finalize().into()
}

/// Verify one `offer` body and refuse a replay of an accepted signature.
/// The crypto check runs first so garbage cannot poison the registry; a
/// replay reads `bad_signature` on the wire and is named `replay` in the log.
pub fn verify_offer_seen(
    devices: &RemoteDevices,
    host_id: &str,
    body: &Value,
    now: i64,
    seen: &mut SeenOffers,
) -> Result<Offer, Refusal> {
    let offer = verify_offer(devices, host_id, body, now)?;
    if seen.check_and_insert(offer_sig_hash(&offer.sig), now) {
        tracing::warn!("remote: replay of a seen offer");
        return Err(Refusal::BadSignature);
    }
    Ok(offer)
}

/// `answer{sdp,sig}`: sign the answer SDP bound to the offer signature.
pub fn answer_body(host: &HostKey, device_id: &str, offer_sig_b64: &str, sdp: &str) -> Value {
    let message = answer_message(device_id, offer_sig_b64, sdp);
    let sig = host.sign(message.as_bytes());
    json!({"t": "answer", "sdp": sdp, "sig": encode_b64(&sig)})
}

/// `answer{sdp,sig}` for a guest offer: guest offers carry no signature to
/// bind to, so the signed payload keeps an empty offer-signature slot.
pub fn guest_answer_body(host: &HostKey, guest_key: &str, sdp: &str) -> Value {
    let message = answer_message(guest_key, "", sdp);
    let sig = host.sign(message.as_bytes());
    json!({"t": "answer", "sdp": sdp, "sig": encode_b64(&sig)})
}

/// One trickled ICE candidate: the object form (`RTCIceCandidateInit`) is
/// canonical, and a legacy bare string reads as a candidate with no mids.
/// Own struct, not `RTCIceCandidateInit`: `peer` owns the `webrtc` boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// The candidate-attribute string.
    pub candidate: String,
    /// The `sdpMid` of the "m=" section, when the sender named one.
    pub sdp_mid: Option<String>,
    /// The `sdpMLineIndex`, when the sender named one.
    pub sdp_mline_index: Option<u16>,
}

impl IceCandidate {
    /// A candidate with no mids, from a legacy bare string or a local gather.
    pub fn bare(candidate: String) -> Self {
        Self {
            candidate,
            sdp_mid: None,
            sdp_mline_index: None,
        }
    }
}

/// `ice{candidate}`: one trickled local candidate, in the object form. The
/// `sdpMid`/`sdpMLineIndex` keys are present only when known; host local
/// candidates carry no mids, so they emit the minimal object.
pub fn ice_body(candidate: &IceCandidate) -> Value {
    let mut inner = json!({"candidate": candidate.candidate});
    if let Some(mid) = &candidate.sdp_mid {
        inner["sdpMid"] = json!(mid);
    }
    if let Some(index) = candidate.sdp_mline_index {
        inner["sdpMLineIndex"] = json!(index);
    }
    json!({"t": "ice", "candidate": inner})
}

/// The candidate of an inbound `ice` body: the object form
/// `{candidate, sdpMid, sdpMLineIndex}` or a legacy bare string. A missing,
/// null or otherwise shaped `candidate` is `None`, never a panic.
pub fn ice_candidate(body: &Value) -> Option<IceCandidate> {
    match body.get("candidate")? {
        Value::String(candidate) => Some(IceCandidate::bare(candidate.clone())),
        Value::Object(fields) => object_candidate(fields),
        _ => None,
    }
}

/// The object-form arm of [`ice_candidate`]: the inner `candidate` string is
/// required, the mids are each `None` unless they spell the right type.
fn object_candidate(fields: &serde_json::Map<String, Value>) -> Option<IceCandidate> {
    let candidate = fields.get("candidate")?.as_str()?.to_string();
    let sdp_mid = fields
        .get("sdpMid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let sdp_mline_index = fields
        .get("sdpMLineIndex")
        .and_then(Value::as_u64)
        .and_then(|index| u16::try_from(index).ok());
    Some(IceCandidate {
        candidate,
        sdp_mid,
        sdp_mline_index,
    })
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
