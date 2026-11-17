//! Retry state for scheduled sends held while the user is typing in the
//! target session. Held sends stay queued untouched until the user is idle.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// How often the delivery task retries held sends.
pub(crate) fn hold_sleep() -> Duration {
    Duration::from_millis(500)
}

/// How long a send stays held before the user gets one desktop notification.
const HELD_NOTIFY_AFTER: Duration = Duration::from_secs(60);

/// Per-send hold state: when each held send was first held, and whether its
/// overdue notification already fired.
#[derive(Debug, Default)]
pub(crate) struct HeldTracker {
    since: HashMap<String, (Instant, bool)>,
}

impl HeldTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.since.is_empty()
    }

    /// Remember a held send, keeping its original hold time across passes.
    pub(crate) fn record_held(&mut self, id: &str) {
        self.since
            .entry(id.to_string())
            .or_insert((Instant::now(), false));
    }

    /// Forget a delivered send.
    pub(crate) fn record_delivered(&mut self, id: &str) {
        self.since.remove(id);
    }

    /// Drop sends that are no longer due (cancelled while held).
    pub(crate) fn retain(&mut self, due_ids: &HashSet<String>) {
        self.since.retain(|id, _| due_ids.contains(id));
    }

    /// True once when `id` has been held past the notify threshold.
    pub(crate) fn held_overdue(&mut self, id: &str) -> bool {
        self.held_overdue_after(id, HELD_NOTIFY_AFTER)
    }

    fn held_overdue_after(&mut self, id: &str, after: Duration) -> bool {
        match self.since.get_mut(id) {
            Some((since, notified)) if !*notified && since.elapsed() >= after => {
                *notified = true;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_send_reports_overdue_exactly_once() {
        let mut tracker = HeldTracker::new();
        assert!(tracker.is_empty());
        tracker.record_held("ss-1");
        assert!(!tracker.is_empty());
        assert!(tracker.held_overdue_after("ss-1", Duration::ZERO));
        assert!(!tracker.held_overdue_after("ss-1", Duration::ZERO));
    }

    #[test]
    fn unknown_send_is_never_overdue() {
        let mut tracker = HeldTracker::new();
        assert!(!tracker.held_overdue_after("missing", Duration::ZERO));
    }

    #[test]
    fn delivered_and_pruned_sends_leave_the_tracker() {
        let mut tracker = HeldTracker::new();
        tracker.record_held("ss-1");
        tracker.record_held("ss-2");
        tracker.record_delivered("ss-1");
        tracker.retain(&HashSet::from(["ss-2".to_string()]));
        assert!(!tracker.is_empty());
        tracker.retain(&HashSet::new());
        assert!(tracker.is_empty());
    }
}
