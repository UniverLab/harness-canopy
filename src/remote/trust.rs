//! What a remote party may see: the owner's everything or a guest's grant.
//!
//! The relay attests the party's GitHub login on `pair_init` and on every
//! offer; the hub binds the login to the device at pair time and re-checks
//! it per offer. This module resolves the login to a [`Grant`] against the
//! trust store — the hub holds no GitHub verifier, so the relay's
//! attestation plus the local trust rows are the whole check.

use std::collections::HashSet;

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
