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

/// Per-session merge windows: for each target session, when its window
/// closes. Seeded fresh every delivery pass from that session's earliest due
/// `fire_at` (`close = earliest + window`); a window never needs to survive
/// a pass because its anchor — the still-queued earliest send — does.
#[derive(Debug, Default)]
pub(crate) struct MergeWindows {
    close: HashMap<String, chrono::DateTime<chrono::Utc>>,
}

impl MergeWindows {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Seed this pass's windows: one close per session from its earliest due
    /// `fire_at`. Clears last pass first, so a session with nothing due
    /// never keeps a stale window.
    pub(crate) fn seed(
        &mut self,
        earliest_per_session: &HashMap<String, chrono::DateTime<chrono::Utc>>,
        window: Duration,
    ) {
        self.close.clear();
        for (session_id, earliest) in earliest_per_session {
            let close = *earliest + chrono::Duration::from_std(window).unwrap_or_default();
            self.close.insert(session_id.clone(), close);
        }
    }

    /// True while `now` is still inside the session's window: its due sends
    /// wait for later ones instead of delivering.
    pub(crate) fn is_open(&self, session_id: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.close.get(session_id).is_some_and(|close| now < *close)
    }

    /// How long until the earliest still-open window closes, so the delivery
    /// task can wake exactly then instead of polling past it.
    pub(crate) fn until_earliest_close(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<Duration> {
        self.close
            .values()
            .filter(|close| **close > now)
            .map(|close| (*close - now).to_std().unwrap_or(Duration::ZERO))
            .min()
    }
}

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
    fn merge_window_opens_at_earliest_fire_at_plus_window() {
        let mut windows = MergeWindows::new();
        let earliest = chrono::Utc::now() - chrono::Duration::seconds(10);
        windows.seed(
            &HashMap::from([("sess".to_string(), earliest)]),
            Duration::from_secs(60),
        );
        assert!(windows.is_open("sess", chrono::Utc::now()));
        assert!(!windows.is_open("sess", earliest + chrono::Duration::seconds(61)));
        assert!(!windows.is_open("other", chrono::Utc::now()));
    }

    #[test]
    fn merge_window_reseed_drops_sessions_with_nothing_due() {
        let mut windows = MergeWindows::new();
        let earliest = chrono::Utc::now();
        windows.seed(
            &HashMap::from([("sess".to_string(), earliest)]),
            Duration::from_secs(60),
        );
        windows.seed(&HashMap::new(), Duration::from_secs(60));
        assert!(!windows.is_open("sess", chrono::Utc::now()));
        assert_eq!(windows.until_earliest_close(chrono::Utc::now()), None);
    }

    #[test]
    fn merge_window_wake_is_the_earliest_open_close() {
        let mut windows = MergeWindows::new();
        let now = chrono::Utc::now();
        windows.seed(
            &HashMap::from([
                ("far".to_string(), now),
                ("near".to_string(), now - chrono::Duration::seconds(50)),
            ]),
            Duration::from_secs(60),
        );
        let wake = windows
            .until_earliest_close(now)
            .expect("an open window sets the wake");
        assert!(wake <= Duration::from_secs(10), "got {wake:?}");
        assert!(wake > Duration::ZERO, "got {wake:?}");
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
