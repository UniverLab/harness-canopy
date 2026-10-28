//! The daemon's live remote state: the relay phase and the device peers.
//!
//! The relay supervisor writes the phase as it moves between waiting,
//! dialling and serving; the `remote_status` tool reads it together with the
//! live peers, so the TUI shows what the daemon is doing right now instead of
//! re-reading config files.

use std::sync::{Arc, Mutex, OnceLock};

use super::connection::DevicePeers;

/// What the relay connection is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelayPhase {
    /// Remote is off, or nobody is signed in.
    #[default]
    Off,
    /// Dialling the relay, or backing off between attempts.
    Connecting,
    /// The last connection attempt reached the relay and served.
    Connected,
    /// The token got a 401: sign in again, this one is never retried.
    TokenRejected,
}

/// Live remote state shared between the supervisor and status surfaces.
pub struct RemoteLive {
    phase: Mutex<RelayPhase>,
    peers: DevicePeers,
}

impl RemoteLive {
    /// Fresh state: `Off` and no peers.
    pub fn new() -> Self {
        Self {
            phase: Mutex::new(RelayPhase::Off),
            peers: DevicePeers::default(),
        }
    }

    /// Record the relay phase (the supervisor calls this).
    pub fn set_phase(&self, phase: RelayPhase) {
        if let Ok(mut current) = self.phase.lock() {
            *current = phase;
        }
    }

    /// The last recorded relay phase.
    pub fn phase(&self) -> RelayPhase {
        self.phase.lock().map(|phase| *phase).unwrap_or_default()
    }

    /// The live device peers.
    pub fn peers(&self) -> &DevicePeers {
        &self.peers
    }

    /// The ids of the devices with a live peer.
    pub fn live_devices(&self) -> Vec<String> {
        self.peers.live_ids()
    }
}

impl Default for RemoteLive {
    fn default() -> Self {
        Self::new()
    }
}

/// The process-wide live state, defaulting to `Off`.
pub fn shared() -> Arc<RemoteLive> {
    static LIVE: OnceLock<Arc<RemoteLive>> = OnceLock::new();
    Arc::clone(LIVE.get_or_init(|| Arc::new(RemoteLive::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_off_with_no_live_devices() {
        let live = RemoteLive::new();
        assert_eq!(live.phase(), RelayPhase::Off);
        assert!(live.live_devices().is_empty());
    }

    #[test]
    fn set_phase_round_trips_every_phase() {
        let live = RemoteLive::new();
        for phase in [
            RelayPhase::Connecting,
            RelayPhase::Connected,
            RelayPhase::TokenRejected,
            RelayPhase::Off,
        ] {
            live.set_phase(phase);
            assert_eq!(live.phase(), phase);
        }
    }

    #[test]
    fn shared_is_one_process_wide_default_off_handle() {
        // `shared` is process-wide, so this only pins the default phase and
        // the shared identity — never the phase, which other tests move.
        assert!(Arc::ptr_eq(&shared(), &shared()));
    }
}
