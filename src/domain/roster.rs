//! CM48: the pure availability decision for one platform/model pair.
//!
//! Both the engine (at dispatch) and `graph_control` call
//! [`effective_state`], so the two can never disagree about whether a pair is
//! runnable. It reads nothing and writes nothing: callers pass the roster rows
//! they already queried, and an injected `now` makes expiry deterministic
//! under test.

use chrono::{DateTime, Utc};

use crate::db::roster_entries::{RosterEntry, RosterState};

/// The effective availability of one platform/model pair at a moment in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub available: bool,
    pub reason: Option<String>,
    pub until: Option<String>,
    pub matched_entry_id: Option<i64>,
    /// The matched entry is an `unavailable` one whose `until` is already in
    /// the past, so the pair reads as available with the expiry visible —
    /// the read surfaces report this rather than silently re-listing it.
    pub expired: bool,
}

impl Availability {
    /// The zero-decision answer: no entry matched, so the pair is available.
    fn unlisted() -> Self {
        Availability {
            available: true,
            reason: None,
            until: None,
            matched_entry_id: None,
            expired: false,
        }
    }

    fn from_entry(entry: &RosterEntry, now: &DateTime<Utc>) -> Self {
        let expired = entry_expired(entry, now);
        Availability {
            available: entry.state == RosterState::Available || expired,
            reason: entry.reason.clone(),
            until: entry.until.clone(),
            matched_entry_id: Some(entry.id),
            expired,
        }
    }
}

/// Parse an `until` timestamp. RFC 3339, normalised to UTC. Shared by
/// `effective_state`'s expiry rule and the `roster_set` validation, so the
/// write edge and the read edge agree about what a valid instant is.
pub(crate) fn parse_until(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// An `unavailable` entry whose `until` is at or before `now`: its pair reads
/// as available again. The one expiry rule — `effective_state` and the
/// `roster_list`/`roster_check` read surfaces report it as expired rather
/// than silently swallowing the entry, and nothing is ever rewritten by a
/// timer.
pub fn entry_expired(entry: &RosterEntry, now: &DateTime<Utc>) -> bool {
    entry.state == RosterState::Unavailable
        && entry
            .until
            .as_deref()
            .and_then(parse_until)
            .is_some_and(|until| until <= *now)
}

/// The prefix a `*`-suffixed `model_pattern` matches on, or `None` when the
/// pattern is not a prefix pattern.
fn prefix_of(pattern: &str) -> Option<&str> {
    pattern.strip_suffix('*')
}

/// Resolve the effective state of `(platform, model)` from `entries`.
///
/// Precedence: an exact model entry beats the longest `*` prefix entry, which
/// beats the platform-wide (`model_pattern IS NULL`) entry. No matching entry
/// means available. An `unavailable` entry whose `until` is at or before `now`
/// is expired and counts as available — expiry is evaluated here, at read
/// time, never by a timer.
pub fn effective_state(
    entries: &[RosterEntry],
    platform: &str,
    model: Option<&str>,
    now: &DateTime<Utc>,
) -> Availability {
    let mut platform_wide: Option<&RosterEntry> = None;
    let mut exact: Option<&RosterEntry> = None;
    let mut best_prefix: Option<(&str, &RosterEntry)> = None;

    for entry in entries.iter().filter(|e| e.platform == platform) {
        match entry.model_pattern.as_deref() {
            None => platform_wide = Some(entry),
            Some(pattern) => {
                let Some(model) = model else { continue };
                if pattern == model {
                    exact = Some(entry);
                } else if let Some(prefix) = prefix_of(pattern) {
                    if model.starts_with(prefix)
                        && best_prefix.is_none_or(|(best, _)| prefix.len() > best.len())
                    {
                        best_prefix = Some((prefix, entry));
                    }
                }
            }
        }
    }

    let winner = exact
        .or_else(|| best_prefix.map(|(_, entry)| entry))
        .or(platform_wide);
    match winner {
        Some(entry) => Availability::from_entry(entry, now),
        None => Availability::unlisted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        id: i64,
        platform: &str,
        pattern: Option<&str>,
        state: RosterState,
        until: Option<&str>,
    ) -> RosterEntry {
        RosterEntry {
            id,
            platform: platform.to_string(),
            model_pattern: pattern.map(str::to_string),
            state,
            reason: Some("test".to_string()),
            until: until.map(str::to_string),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-06-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn exact_overrides_prefix_and_platform() {
        let entries = vec![
            entry(1, "claude", None, RosterState::Unavailable, None),
            entry(2, "claude", Some("op*"), RosterState::Available, None),
            entry(3, "claude", Some("opus"), RosterState::Unavailable, None),
        ];
        let got = effective_state(&entries, "claude", Some("opus"), &now());
        assert!(!got.available);
        assert_eq!(got.matched_entry_id, Some(3));
    }

    #[test]
    fn longest_prefix_wins() {
        let entries = vec![
            entry(1, "claude", Some("claude*"), RosterState::Unavailable, None),
            entry(2, "claude", Some("claude-4*"), RosterState::Available, None),
        ];
        let got = effective_state(&entries, "claude", Some("claude-4-sonnet"), &now());
        assert!(got.available, "longer prefix entry must win");
        assert_eq!(got.matched_entry_id, Some(2));
    }

    #[test]
    fn prefix_over_platform() {
        let entries = vec![
            entry(1, "opencode-go", None, RosterState::Unavailable, None),
            entry(
                2,
                "opencode-go",
                Some("opencode-go/*"),
                RosterState::Available,
                None,
            ),
        ];
        let got = effective_state(&entries, "opencode-go", Some("opencode-go/claude"), &now());
        assert!(got.available, "prefix must beat platform-wide");
        assert_eq!(got.matched_entry_id, Some(2));
    }

    #[test]
    fn expired_until_counts_as_available() {
        let entries = vec![entry(
            1,
            "opencode-go",
            None,
            RosterState::Unavailable,
            Some("2026-05-01T00:00:00Z"),
        )];
        let got = effective_state(&entries, "opencode-go", None, &now());
        assert!(got.available, "expired entry reads as available");
        assert!(got.expired, "the expiry must be reported, not swallowed");
        assert_eq!(got.matched_entry_id, Some(1));
        assert_eq!(got.until.as_deref(), Some("2026-05-01T00:00:00Z"));
    }

    #[test]
    fn future_until_stays_unavailable() {
        let entries = vec![entry(
            1,
            "opencode-go",
            None,
            RosterState::Unavailable,
            Some("2026-07-01T00:00:00Z"),
        )];
        let got = effective_state(&entries, "opencode-go", None, &now());
        assert!(!got.available);
    }

    #[test]
    fn no_entry_means_available() {
        let got = effective_state(&[], "claude", Some("opus"), &now());
        assert!(got.available);
        assert_eq!(got.matched_entry_id, None);
    }

    #[test]
    fn available_state_with_until_is_available() {
        let entries = vec![entry(
            1,
            "claude",
            None,
            RosterState::Available,
            Some("2026-01-01T00:00:00Z"),
        )];
        let got = effective_state(&entries, "claude", None, &now());
        assert!(got.available, "until is ignored for an available entry");
    }

    #[test]
    fn other_platform_entries_are_ignored() {
        let entries = vec![entry(1, "claude", None, RosterState::Unavailable, None)];
        let got = effective_state(&entries, "opencode-go", None, &now());
        assert!(got.available);
        assert_eq!(got.matched_entry_id, None);
    }
}
