//! Process-local "the daemon is stopping" flag for the session hub.
//!
//! While it is set, a session child that exits must keep its database row as
//! it was (`active` for an agent, `idle` for a terminal): the next daemon
//! start resumes it. The flag lives only in this process and is never
//! persisted — a session the user really closed, or one whose harness
//! crashed during normal operation, still finishes its row as always.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set once, when the daemon's stop signal arrives and before the serve loop
/// drains, so every exit recorded after that point keeps its row resumable.
#[derive(Debug, Default)]
pub struct HubShutdown {
    set: AtomicBool,
}

impl HubShutdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the daemon as shutting down. Idempotent.
    pub fn set(&self) {
        self.set.store(true, Ordering::SeqCst);
    }

    /// Whether the daemon is shutting down.
    pub fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_starts_clear_and_stays_set_once_set() {
        let flag = HubShutdown::new();
        assert!(!flag.is_set());
        flag.set();
        assert!(flag.is_set());
        flag.set();
        assert!(flag.is_set(), "setting is idempotent");
    }
}
