//! Pending editor-draft bodies fetched for review but not yet installed.
//!
//! A draft is deleted by its first install — in practice, by the first GET:
//! the catalogue serves the document once and a second fetch 404s. The MCP
//! `graph_edit`/`node` tools fetch once per call (first for the
//! review, then again after `accept: true`), so the review call parks its
//! body here and the accept call reuses it; without the cache `--yes` could
//! never install a draft. Entries expire after 30 minutes and the map is
//! capped, so a forgotten review never leaks memory.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Most draft bodies parked at once.
const MAX_ENTRIES: usize = 32;

/// How long a parked body stays reusable: the draft's own lifetime.
const TTL: Duration = Duration::from_secs(30 * 60);

struct PendingDraft {
    body: String,
    source_url: String,
    stored_at: Instant,
}

static PENDING: OnceLock<Mutex<HashMap<String, PendingDraft>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<String, PendingDraft>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Park the body (and the URL it was read from) the review call fetched for
/// draft `id`, so the accept call can install without re-fetching.
pub(crate) fn store(id: &str, body: String, source_url: String) {
    let mut map = pending().lock().expect("draft cache lock");
    map.retain(|_, entry| entry.stored_at.elapsed() < TTL);
    if map.len() >= MAX_ENTRIES {
        evict_oldest(&mut map);
    }
    map.insert(
        id.to_string(),
        PendingDraft {
            body,
            source_url,
            stored_at: Instant::now(),
        },
    );
}

/// Take the parked body for draft `id`, if it is still fresh. A taken (or
/// expired, or never-stored) draft returns `None` and the caller re-fetches —
/// the catalogue's 404 then says the draft is gone.
pub(crate) fn take(id: &str) -> Option<(String, String)> {
    let mut map = pending().lock().expect("draft cache lock");
    let entry = map.remove(id)?;
    if entry.stored_at.elapsed() < TTL {
        Some((entry.body, entry.source_url))
    } else {
        None
    }
}

/// Drop the stalest entry so the map stays capped.
fn evict_oldest(map: &mut HashMap<String, PendingDraft>) {
    let oldest = map
        .iter()
        .min_by_key(|(_, entry)| entry.stored_at)
        .map(|(id, _)| id.clone());
    if let Some(id) = oldest {
        map.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_then_take_returns_the_body_and_url_once() {
        let id = "cache-roundtrip-00000000000001";
        store(id, "the body".to_string(), "the url".to_string());
        assert_eq!(
            take(id),
            Some(("the body".to_string(), "the url".to_string()))
        );
        assert_eq!(
            take(id),
            None,
            "a taken draft is consumed, like the catalogue's own"
        );
    }

    #[test]
    fn take_returns_none_for_a_draft_never_stored() {
        assert_eq!(take("cache-never-stored-00000001"), None);
    }

    #[test]
    fn the_map_keeps_only_the_cap_when_overfilled() {
        for index in 0..(MAX_ENTRIES + 8) {
            let id = format!("cache-cap-{index:026}");
            store(&id, "body".to_string(), "url".to_string());
        }
        let mut hits = 0;
        for index in 0..(MAX_ENTRIES + 8) {
            let id = format!("cache-cap-{index:026}");
            if take(&id).is_some() {
                hits += 1;
            }
        }
        assert_eq!(hits, MAX_ENTRIES, "only the capped entries survive");
    }
}
