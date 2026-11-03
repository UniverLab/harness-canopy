//! Answering offers: the device path and the guest path.
//!
//! Split from [`super::relay`] for the file-size gate: both handlers are
//! [`RelayRuntime`] methods (same crate, so the inherent impl can live
//! here) over the runtime's `pub(crate)` fields. Bodies moved verbatim —
//! this module changes no behavior, only the address.

use std::sync::Arc;

use serde_json::Value;

use super::connection::{self, PeerSession};
use super::peer;
use super::relay::{RelayRuntime, SignalTx};
use super::signaling::{self, Refusal};
use super::trust;

impl RelayRuntime {
    /// Verify an offer, refuse it, or build the answer off the socket loop.
    pub(crate) fn start_offer(&self, body: &Value, now: i64, from: &str, signal: &SignalTx) {
        let host_id = self.host.host_id();
        let mut seen = self
            .seen_offers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let offer =
            match signaling::verify_offer_seen(&self.devices, &host_id, body, now, &mut seen) {
                Ok(offer) => offer,
                Err(refusal) => {
                    let _ = signal.send((from.to_string(), signaling::connect_fail(refusal)));
                    return;
                }
            };
        drop(seen);
        // The hub is the enforcement point: a login without a grant never
        // gets a peer, so untrusted guests cannot even occupy a slot.
        let Some(grant) = trust::resolve(&self.db, &self.github_login, &offer.device.github_login)
        else {
            let _ = signal.send((
                from.to_string(),
                signaling::connect_fail(Refusal::Forbidden),
            ));
            return;
        };
        let can_upload =
            trust::resolve_upload(&self.db, &self.github_login, &offer.device.github_login);
        let Some(sessions) = self.sessions.clone() else {
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        };
        let device_id = offer.device.id.clone();
        let token = self.connections.register(&device_id);
        let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
        if !self
            .peers
            .register(&device_id, from, Arc::clone(&token), control_tx)
        {
            self.connections.release(&device_id, &token);
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        }
        let host = Arc::clone(&self.host);
        let devices = self.devices.clone();
        let peers = self.peers.clone();
        let connections = self.connections.clone();
        let relay_url = self.relay_url.clone();
        let bearer = self.bearer.clone();
        let signal = signal.clone();
        let from = from.to_string();
        tokio::spawn(async move {
            let servers = peer::fetch_ice_servers_or_fallback(&relay_url, &bearer).await;
            let answered = peer::Peer::answer_offer(servers, &offer.sdp).await;
            let Ok((sdp, peer)) = answered else {
                peers.remove(&device_id, &token);
                connections.release(&device_id, &token);
                let _ = signal.send((from, signaling::connect_fail(Refusal::BadSignature)));
                return;
            };
            let answer = signaling::answer_body(&host, &device_id, &offer.sig_b64, &sdp);
            let _ = signal.send((from.clone(), answer));
            let _ = devices.touch(&device_id);
            connection::spawn(PeerSession {
                peer,
                device_id,
                state: sessions,
                peers,
                connections,
                token,
                control: control_rx,
                signal,
                from,
                grant,
                can_upload,
                max_frame: connection::peer_frame_cap(&offer.sdp),
            });
        });
    }

    /// Verify a guest offer, refuse it, or build the answer off the socket
    /// loop. The guest path needs no device row and no replay registry
    /// (there is no signature to key on): injected identity plus a live
    /// grant. The peer is keyed `guest-{uid}` and skips the `last_seen`
    /// touch, which belongs to the device registry.
    pub(crate) fn start_guest_offer(&self, body: &Value, now: i64, from: &str, signal: &SignalTx) {
        let offer = match self
            .guests
            .verify_guest_offer(&self.db, &self.github_login, body, from)
        {
            Ok(offer) => offer,
            Err(refusal) => {
                let _ = signal.send((from.to_string(), signaling::connect_fail(refusal)));
                return;
            }
        };
        // The first accepted offer redeems the invite: single-use from here.
        let _ = self.db.consume_invites_for_uid(offer.uid, now);
        let Some(sessions) = self.sessions.clone() else {
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        };
        let guest_key = format!("guest-{}", offer.uid);
        let token = self.connections.register(&guest_key);
        let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
        if !self
            .peers
            .register(&guest_key, from, Arc::clone(&token), control_tx)
        {
            self.connections.release(&guest_key, &token);
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        }
        let host = Arc::clone(&self.host);
        let peers = self.peers.clone();
        let connections = self.connections.clone();
        let relay_url = self.relay_url.clone();
        let bearer = self.bearer.clone();
        let signal = signal.clone();
        let from = from.to_string();
        tokio::spawn(async move {
            let servers = peer::fetch_ice_servers_or_fallback(&relay_url, &bearer).await;
            let answered = peer::Peer::answer_offer(servers, &offer.sdp).await;
            let Ok((sdp, peer)) = answered else {
                peers.remove(&guest_key, &token);
                connections.release(&guest_key, &token);
                let _ = signal.send((from, signaling::connect_fail(Refusal::BadSignature)));
                return;
            };
            let answer = signaling::guest_answer_body(&host, &guest_key, &sdp);
            let _ = signal.send((from.clone(), answer));
            connection::spawn(PeerSession {
                peer,
                device_id: guest_key,
                state: sessions,
                peers,
                connections,
                token,
                control: control_rx,
                signal,
                from,
                grant: offer.grant,
                can_upload: offer.upload,
                max_frame: connection::peer_frame_cap(&offer.sdp),
            });
        });
    }
}
