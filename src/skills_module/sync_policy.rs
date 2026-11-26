//! Content-hash helpers for skills cleanup.
//!
//! An earlier incident showed that mtimes cannot be trusted as a divergence
//! signal: a plain file copy preserves the source mtime, so a stale copy can
//! look "unchanged" even though its content is old. Divergence is therefore
//! always decided by hashing file content, never by timestamps.
//! The cleanup in [`crate::skills_module::cleanup`] uses this to compare
//! legacy `~/.agents/skills/` copies against upstream bytes.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncAction {
    /// Destination doesn't exist yet, or `force` is set: write incoming content.
    Write,
    /// Destination already matches incoming content byte-for-byte: nothing to do.
    Unchanged,
    /// Destination diverges from incoming content and `force` is not set: skip.
    Skip,
}

/// SHA-256 of `content`, the identity the cleanup compares by (never
/// GitHub's SHA-1 blob `sha`, which hashes a different object).
pub(crate) fn hash(content: &[u8]) -> [u8; 32] {
    Sha256::digest(content).into()
}

/// Decide what to do about writing `incoming` over `existing` (the current
/// destination content, or `None` if the destination doesn't exist yet).
///
/// Pure and filesystem-free so the decision matrix can be unit tested directly.
pub(crate) fn decide_sync_action(
    existing: Option<&[u8]>,
    incoming: &[u8],
    force: bool,
) -> SyncAction {
    match existing {
        None => SyncAction::Write,
        Some(_) if force => SyncAction::Write,
        Some(existing) if hash(existing) == hash(incoming) => SyncAction::Unchanged,
        Some(_) => SyncAction::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_when_destination_missing() {
        assert_eq!(decide_sync_action(None, b"new", false), SyncAction::Write);
    }

    #[test]
    fn skips_when_content_diverges() {
        assert_eq!(
            decide_sync_action(Some(b"local v1.7"), b"stale v1.2", false),
            SyncAction::Skip
        );
    }

    #[test]
    fn unchanged_when_content_identical() {
        assert_eq!(
            decide_sync_action(Some(b"same"), b"same", false),
            SyncAction::Unchanged
        );
    }

    #[test]
    fn force_overwrites_diverged_content() {
        assert_eq!(
            decide_sync_action(Some(b"local v1.7"), b"stale v1.2", true),
            SyncAction::Write
        );
    }

    #[test]
    fn force_is_a_noop_when_content_already_matches() {
        // Force still results in a write, which is fine — it's a byte-identical
        // rewrite. Documented here so the behavior doesn't drift silently.
        assert_eq!(
            decide_sync_action(Some(b"same"), b"same", true),
            SyncAction::Write
        );
    }

    #[test]
    fn hash_empty_content() {
        let h = hash(b"");
        assert_eq!(h.len(), 32);
        // SHA-256 of empty string is well-known
        let expected: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(h, expected);
    }

    #[test]
    fn hash_deterministic() {
        let h1 = hash(b"hello world");
        let h2 = hash(b"hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_different_for_different_content() {
        let h1 = hash(b"content A");
        let h2 = hash(b"content B");
        assert_ne!(h1, h2);
    }

    #[test]
    fn decide_sync_action_force_on_existing_identical() {
        assert_eq!(
            decide_sync_action(Some(b"same"), b"same", true),
            SyncAction::Write
        );
    }

    #[test]
    fn decide_sync_action_no_force_on_missing() {
        assert_eq!(decide_sync_action(None, b"new", false), SyncAction::Write);
    }

    #[test]
    fn decide_sync_action_force_on_missing() {
        assert_eq!(decide_sync_action(None, b"new", true), SyncAction::Write);
    }

    #[test]
    fn sync_action_debug_trait() {
        // Ensure SyncAction can be debug-formatted (derives Debug)
        assert_eq!(format!("{:?}", SyncAction::Write), "Write");
        assert_eq!(format!("{:?}", SyncAction::Unchanged), "Unchanged");
        assert_eq!(format!("{:?}", SyncAction::Skip), "Skip");
    }

    #[test]
    fn sync_action_equality() {
        assert_eq!(SyncAction::Write, SyncAction::Write);
        assert_ne!(SyncAction::Write, SyncAction::Skip);
        assert_ne!(SyncAction::Unchanged, SyncAction::Skip);
    }
}
