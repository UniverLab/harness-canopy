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

/// Whether `login` is the host owner's login (trimmed, case-insensitive).
/// Blank logins never are.
pub fn is_owner_login(owner_login: &str, login: &str) -> bool {
    let login = normalize_login(login);
    !login.is_empty() && login == normalize_login(owner_login)
}

/// Resolve a party's upload permission: the owner may always upload, a
/// trusted login gets exactly its stored `upload` flag (default deny), and
/// anything else (unknown, blank, unreadable store) is refused.
pub fn resolve_upload(db: &Database, owner_login: &str, guest_login: &str) -> bool {
    let guest = normalize_login(guest_login);
    if guest.is_empty() {
        return false;
    }
    if guest == normalize_login(owner_login) {
        return true;
    }
    db.get_trust(&guest)
        .ok()
        .flatten()
        .is_some_and(|profile| profile.upload)
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
    can_upload: bool,
    owner: bool,
}

impl GrantScope {
    /// Scope a peer to its resolved grant.
    pub fn new(grant: Grant) -> Self {
        Self {
            grant,
            visible: HashSet::new(),
            can_upload: false,
            owner: false,
        }
    }

    /// Scope a peer to everything (the owner, or a full grant).
    pub fn all() -> Self {
        let mut scope = Self::new(Grant::All);
        scope.can_upload = true;
        scope
    }

    /// Record whether this peer holds the `upload` grant.
    pub fn set_can_upload(&mut self, can_upload: bool) {
        self.can_upload = can_upload;
    }

    /// Whether this peer may upload files into its granted sessions.
    pub fn can_upload(&self) -> bool {
        self.can_upload
    }

    /// Record whether this peer is the host owner's own paired device.
    pub fn set_owner(&mut self, owner: bool) {
        self.owner = owner;
    }

    /// Whether this peer is the host owner's own paired device: the only
    /// remote party that may size a session's PTY.
    pub fn is_owner(&self) -> bool {
        self.owner
    }

    /// Whether the grant covers every session (the owner, or a full grant).
    pub fn is_all(&self) -> bool {
        matches!(self.grant, Grant::All)
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
    /// the grant, or it is a host-shelf file (`None`, tied to no session)
    /// and the peer's grant is narrower than every session — the host shelf
    /// is the owner's (and full grants'), never a session guest's.
    pub fn hidden_file(&self, hub: &SessionHub, session_id: Option<&str>) -> bool {
        match session_id {
            Some(id) => !self.allows_id(hub, id),
            None => !self.is_all(),
        }
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

    /// Whether a shelf file hides from a scoped peer, by
    /// [`hidden_file`](Self::hidden_file)'s rule. Unknown files are not
    /// hidden here — the delete path answers `not_found` for those itself.
    pub fn hidden_shelf_file(&self, hub: &SessionHub, shelf: &ShelfStore, file_id: &str) -> bool {
        shelf
            .get(file_id)
            .is_some_and(|row| self.hidden_file(hub, row.session_id.as_deref()))
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
