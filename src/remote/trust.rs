//! What a remote party may see: the owner's everything or a guest's grant.
//!
//! The party's GitHub login arrives with every offer — injected by the relay
//! on the guest path, a client hint checked against the pair-time binding on
//! the device path. This module resolves the login to a [`Grant`] against
//! the trust store — the hub holds no GitHub verifier, so the login plus the
//! local trust rows are the whole check.

use std::collections::HashSet;

use serde_json::{json, Value};

use super::invite::PublishedInvite;
use crate::db::Database;
use crate::sessions::protocol::{error_frame, ERR_NOT_FOUND};
use crate::sessions::shelf::ShelfStore;
use crate::sessions::ws::{no_such_session, OutTx};
use crate::sessions::SessionHub;

/// The sessions one remote party may interact with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// Every session, present and future (the owner, or a full grant).
    All,
    /// Only the sessions with these names.
    Sessions(HashSet<String>),
}

impl Grant {
    /// Whether a session with this name is inside the grant.
    pub fn allows(&self, session_name: &str) -> bool {
        match self {
            Grant::All => true,
            Grant::Sessions(names) => names.contains(session_name),
        }
    }
}

/// Canonical login form: trimmed and lowercased.
pub fn normalize_login(login: &str) -> String {
    login.trim().to_lowercase()
}

/// Resolve a party's grant: the owner is always [`Grant::All`] with no trust
/// row needed, a trusted login resolves to its stored grant, anything else
/// (unknown, blank, or unreadable store) resolves to nothing — fail closed.
pub fn resolve(db: &Database, owner_login: &str, guest_login: &str) -> Option<Grant> {
    let guest = normalize_login(guest_login);
    if guest.is_empty() {
        return None;
    }
    if guest == normalize_login(owner_login) {
        return Some(Grant::All);
    }
    let profile = db.get_trust(&guest).ok()??;
    if profile.all_sessions {
        return Some(Grant::All);
    }
    Some(Grant::Sessions(profile.sessions.into_iter().collect()))
}

/// Build the room's `guest_allow` body for these uids and live invites:
/// sorted, deduped, and byte-exact — the worker matches the shape verbatim,
/// an empty uid list is a present deny-all (explicit beats absent), and the
/// `invites` array is always present (`[]` when none is live).
pub fn guest_allow_body(uids: &[u64], invites: &[PublishedInvite]) -> Value {
    let mut sorted: Vec<u64> = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut live: Vec<&PublishedInvite> = invites.iter().collect();
    live.sort_by(|left, right| left.code_hash.cmp(&right.code_hash));
    live.dedup_by(|left, right| left.code_hash == right.code_hash);
    let invites: Vec<Value> = live
        .into_iter()
        .map(|invite| {
            json!({
                "code_hash": invite.code_hash,
                "uid": invite.uid,
                "expires": invite.expires,
            })
        })
        .collect();
    json!({"t": "guest_allow", "uids": sorted, "invites": invites})
}

/// Fingerprint the trust allowlist for change detection: the trusted logins
/// in store order, joined with `,`; `"error"` when the store is unreadable,
/// so a failure still publishes (deny-all) exactly once. While any invite
/// row exists, `|<hash>:<used>,…` (store order) follows, so issuing,
/// redeeming or revoking an invite republishes the room's allowlist.
pub fn allowlist_fingerprint(db: &Database) -> String {
    let trust = match db.list_trust() {
        Ok(profiles) => profiles
            .iter()
            .map(|profile| profile.login.as_str())
            .collect::<Vec<_>>()
            .join(","),
        Err(_) => "error".to_string(),
    };
    match db.list_invites() {
        Ok(rows) if rows.is_empty() => trust,
        Ok(rows) => format!(
            "{trust}|{}",
            rows.iter()
                .map(|row| format!("{}:{}", row.code_hash, i64::from(row.used)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Err(_) => format!("{trust}|error"),
    }
}

/// One peer's enforcement view: its [`Grant`] plus the granted session ids
/// it has seen (listed or announced), so a later `session_removed`/`exited`
/// for one still forwards while an ungranted session's never does.
#[derive(Debug, Clone)]
pub struct GrantScope {
    grant: Grant,
    visible: HashSet<String>,
}

impl GrantScope {
    /// Scope a peer to its resolved grant.
    pub fn new(grant: Grant) -> Self {
        Self {
            grant,
            visible: HashSet::new(),
        }
    }

    /// Scope a peer to everything (the owner, or a full grant).
    pub fn all() -> Self {
        Self::new(Grant::All)
    }

    /// Whether a session name is inside the grant.
    pub fn allows(&self, session_name: &str) -> bool {
        self.grant.allows(session_name)
    }

    /// Whether `id` is inside the grant: everything for [`Grant::All`] (a
    /// missing id falls through to the handler's own `not_found`, exactly
    /// like before grants), else a live session with a granted name.
    pub fn allows_id(&self, hub: &SessionHub, id: &str) -> bool {
        match self.grant {
            Grant::All => true,
            Grant::Sessions(_) => hub
                .get(id)
                .is_some_and(|session| self.allows(&session.name)),
        }
    }

    /// A file hidden from a scoped peer: it belongs to a session outside
    /// the grant. Host files (`None`) stay visible to every peer.
    pub fn hidden_file(&self, hub: &SessionHub, session_id: Option<&str>) -> bool {
        session_id.is_some_and(|id| !self.allows_id(hub, id))
    }

    /// Pass when `id` is inside the grant, else answer `not_found` like a
    /// missing session. Runs before the terminal check, so an ungranted
    /// terminal looks exactly like a missing session.
    pub fn granted(&self, hub: &SessionHub, req: Option<&str>, id: &str, out: &OutTx) -> bool {
        if self.allows_id(hub, id) {
            return true;
        }
        let _ = out.send(error_frame(req, ERR_NOT_FOUND, &no_such_session(id)));
        false
    }

    /// Whether a shelf file hides from a scoped peer: it belongs to a
    /// session outside the grant. Unknown files are not hidden here — the
    /// delete path answers `not_found` for those itself.
    pub fn hidden_shelf_file(&self, hub: &SessionHub, shelf: &ShelfStore, file_id: &str) -> bool {
        shelf
            .get(file_id)
            .as_ref()
            .and_then(|row| row.session_id.as_deref())
            .is_some_and(|id| !self.allows_id(hub, id))
    }

    /// Remember a listed or announced session; whether it is inside the
    /// grant (and so forwards to the peer).
    pub fn note_listed(&mut self, id: &str, name: &str) -> bool {
        if !self.allows(name) {
            return false;
        }
        self.visible.insert(id.to_string());
        true
    }

    /// Whether a removal forwards: always for [`Grant::All`], else only for
    /// a session the peer saw or attached to.
    pub fn removed_forwards(&mut self, id: &str, was_attached: bool) -> bool {
        let was_visible = self.visible.remove(id);
        matches!(self.grant, Grant::All) || was_attached || was_visible
    }

    /// Whether an exit forwards: a broadcast or an attachment for
    /// [`Grant::All`], else only for a session the peer saw or attached to.
    pub fn exited_forwards(&self, id: &str, to_all: bool, attached: bool) -> bool {
        match self.grant {
            Grant::All => to_all || attached,
            Grant::Sessions(_) => attached || self.visible.contains(id),
        }
    }
}

#[cfg(test)]
#[path = "grant_tests.rs"]
mod tests;
