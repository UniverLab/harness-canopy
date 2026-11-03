#[cfg(test)]
mod binding_test;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

//  Channel number:
//    0x4000 through 0x7FFF: These values are the allowed channel
//    numbers (16,383 possible values).
const MIN_CHANNEL_NUMBER: u16 = 0x4000;
const MAX_CHANNEL_NUMBER: u16 = 0x7fff;
const CHANNEL_NUMBER_COUNT: u16 = MAX_CHANNEL_NUMBER - MIN_CHANNEL_NUMBER + 1;
// canopy patch: RFC 8656 §12 keeps a channel bound for 10 minutes unless the
// client refreshes it. Bindings live at least that long so a channel the
// server still holds keeps resolving incoming ChannelData.
pub(crate) const CHANNEL_LIFETIME: Duration = Duration::from_secs(600);

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum BindingState {
    Idle,
    Request,
    Ready,
    Refresh,
    Failed,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Binding {
    pub(crate) number: u16,
    pub(crate) st: BindingState,
    pub(crate) addr: SocketAddr,
    pub(crate) refreshed_at: Instant,
    // canopy patch: how many stale-nonce retries of this bind have been sent.
    pub(crate) bind_retries: u8,
    // canopy patch: whether this binding's failure has already been logged at
    // WARN, so later repeats of the same failure go to DEBUG.
    pub(crate) warned: bool,
    // canopy patch: when the last CHANNEL-BIND for this binding was sent to
    // the server; the channel lifetime is measured from here.
    pub(crate) sent_at: Instant,
}

impl Binding {
    pub(crate) fn set_state(&mut self, state: BindingState) {
        self.st = state;
    }

    pub(crate) fn state(&self) -> BindingState {
        self.st
    }

    pub(crate) fn set_refreshed_at(&mut self, at: Instant) {
        self.refreshed_at = at;
    }

    pub(crate) fn refreshed_at(&self) -> Instant {
        self.refreshed_at
    }
}
/// Thread-safe Binding map.
#[derive(Default)]
pub(crate) struct BindingManager {
    chan_map: HashMap<u16, String>,
    addr_map: HashMap<String, Binding>,
    next: u16,
}

impl BindingManager {
    pub(crate) fn new() -> Self {
        BindingManager {
            chan_map: HashMap::new(),
            addr_map: HashMap::new(),
            next: MIN_CHANNEL_NUMBER,
        }
    }

    pub(crate) fn assign_channel_number(&mut self) -> u16 {
        // canopy patch: a number already bound to another peer must never be
        // handed out again — that is exactly the 400 RFC 8656 §12 requires the
        // server to answer. Bindings are kept across failures now, so walk past
        // every number still held by the channel map.
        for _ in 0..CHANNEL_NUMBER_COUNT {
            let n = self.next;
            if self.next == MAX_CHANNEL_NUMBER {
                self.next = MIN_CHANNEL_NUMBER;
            } else {
                self.next += 1;
            }
            if !self.chan_map.contains_key(&n) {
                return n;
            }
        }
        // Every channel number is in use; one binding per peer means this
        // cannot happen for a valid peer set, so keep upstream behaviour.
        self.next
    }

    pub(crate) fn create(&mut self, now: Instant, addr: SocketAddr) -> Option<&Binding> {
        // canopy patch: bindings are never deleted on a bind error any more,
        // so drop the ones whose channel the server's lifetime has expired.
        self.prune_expired(now);

        let b = Binding {
            number: self.assign_channel_number(),
            st: BindingState::Idle,
            addr,
            refreshed_at: now,
            bind_retries: 0,
            warned: false,
            sent_at: now,
        };

        self.chan_map.insert(b.number, b.addr.to_string());
        self.addr_map.insert(b.addr.to_string(), b);
        self.addr_map.get(&addr.to_string())
    }

    /// canopy patch: forget bindings whose last CHANNEL-BIND is older than the
    /// RFC channel lifetime. The server has reclaimed the channel by then, so
    /// the number is no longer owed to that peer and may be reused.
    pub(crate) fn prune_expired(&mut self, now: Instant) {
        let expired: Vec<SocketAddr> = self
            .addr_map
            .values()
            .filter(|b| now.saturating_duration_since(b.sent_at) >= CHANNEL_LIFETIME)
            .map(|b| b.addr)
            .collect();
        for addr in expired {
            self.delete_by_addr(&addr);
        }
    }

    pub(crate) fn find_by_addr(&self, addr: &SocketAddr) -> Option<&Binding> {
        self.addr_map.get(&addr.to_string())
    }

    pub(crate) fn get_by_addr(&mut self, addr: &SocketAddr) -> Option<&mut Binding> {
        self.addr_map.get_mut(&addr.to_string())
    }

    pub(crate) fn find_by_number(&self, number: u16) -> Option<&Binding> {
        let s = self.chan_map.get(&number)?;
        self.addr_map.get(s)
    }

    pub(crate) fn get_by_number(&mut self, number: u16) -> Option<&mut Binding> {
        let s = self.chan_map.get(&number)?;
        self.addr_map.get_mut(s)
    }

    pub(crate) fn delete_by_addr(&mut self, addr: &SocketAddr) -> bool {
        if let Some(b) = self.addr_map.remove(&addr.to_string()) {
            self.chan_map.remove(&b.number);
            true
        } else {
            false
        }
    }

    pub(crate) fn delete_by_number(&mut self, number: u16) -> bool {
        if let Some(s) = self.chan_map.remove(&number) {
            self.addr_map.remove(&s);
            true
        } else {
            false
        }
    }

    pub(crate) fn size(&self) -> usize {
        self.addr_map.len()
    }
}
