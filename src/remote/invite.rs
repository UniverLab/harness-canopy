//! Guest invite codes: host-issued, single-use, hash-only at rest.
//!
//! A guest has no paired device, so it cannot `pair_init` and owns no device
//! row. The host instead issues it an invite code — `CNR-` plus 16 Crockford
//! base32 characters (80-bit crypto-random) — and publishes only the
//! code's SHA256 hex to the room's `guest_allow` frame. The relay checks the
//! guest's `invite=<code>` door parameter against the published hashes and
//! the guest's own session uid; the host never sees the code again after
//! printing it once.
//!
//! Invites are single-use: the first accepted guest offer burns every live
//! invite for that uid ([`crate::db::Database::consume_invites_for_uid`]),
//! so a shared code opens nothing afterwards.

use anyhow::{bail, Result};
use rand::{rngs::SysRng, TryRng as _};
use sha2::{Digest as _, Sha256};

use super::pairing::ALPHABET;
use super::trust;
use crate::db::Database;

/// Days an invite stays live when the issuer names no other lifetime.
pub const DEFAULT_DAYS: i64 = 7;
/// Seconds in one invite day.
const DAY_SECONDS: i64 = 86_400;
/// Random bytes behind one code: 80 bits, shown as 16 five-bit symbols.
const CODE_BYTES: usize = 10;
/// Symbols in one code (without the `CNR-` prefix and dashes).
const CODE_SYMBOLS: usize = 16;

/// A freshly issued invite: the code in cleartext, printed once and never
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedInvite {
    /// Formatted as `CNR-XXXX-XXXX-XXXX-XXXX`.
    pub code: String,
    /// Unix seconds when the invite stops opening the door.
    pub expires_at: i64,
}

/// One live invite as the `guest_allow` publish carries it: hash only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedInvite {
    /// SHA256 hex of the canonical code.
    pub code_hash: String,
    /// The invited login's GitHub uid.
    pub uid: u64,
    /// Unix seconds when the invite stops opening the door.
    pub expires: i64,
}

/// Issue an invite for `login` (already uid-resolved by the caller), live
/// for `days` days from `now`. Stores the code's hash only; the returned
/// cleartext is printed once by the CLI and never logged.
pub fn issue(db: &Database, login: &str, uid: u64, now: i64, days: i64) -> Result<IssuedInvite> {
    let login = trust::normalize_login(login);
    if login.is_empty() {
        bail!("invite login must not be blank");
    }
    let code = generate_code()?;
    let expires_at = now.saturating_add(days.saturating_mul(DAY_SECONDS));
    db.insert_invite(&code_hash(&code), &login, uid, expires_at)?;
    Ok(IssuedInvite { code, expires_at })
}

/// Every live invite (unused and unexpired) at `now`, ascending by hash so
/// the publish is byte-stable. An unreadable store reads as no invites —
/// fail closed, like the uid allowlist.
pub fn live(db: &Database, now: i64) -> Vec<PublishedInvite> {
    let rows = match db.list_invites() {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!("remote: invites: cannot list the invite store: {error:#}");
            return Vec::new();
        }
    };
    let mut out: Vec<PublishedInvite> = rows
        .into_iter()
        .filter(|row| !row.used && row.expires_at > now)
        .map(|row| PublishedInvite {
            code_hash: row.code_hash,
            uid: row.uid,
            expires: row.expires_at,
        })
        .collect();
    out.sort_by(|left, right| left.code_hash.cmp(&right.code_hash));
    out
}

/// SHA256 hex (lowercase) of the canonical code: the only form ever stored
/// or published.
pub fn code_hash(canonical: &str) -> String {
    Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Parse hand-typed input back to the canonical `CNR-…` form: case and
/// dashes/spaces ignored, Crockford confusables folded (`I`/`L`→`1`,
/// `O`→`0`, like [`super::pairing::normalize_code`]), the `CNR` prefix
/// optional. `None` when the input is not a code at all.
pub fn normalize_code_input(input: &str) -> Option<String> {
    let mut core: String = input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect();
    if core.len() == 3 + CODE_SYMBOLS && core.starts_with("CNR") {
        core = core[3..].to_string();
    }
    if core.len() != CODE_SYMBOLS || !core.bytes().all(|byte| ALPHABET.contains(&byte)) {
        return None;
    }
    Some(format_code(&core))
}

/// The guest link for one invite: `<relay_url>/g/<owner_uid>/<host_id>`
/// with the code in the fragment (`#invite=<code>`), which browsers never
/// send to the relay or put in a `Referer`; the guest page reads it, keeps
/// it for that tab only and strips it from the address bar. The relay URL
/// is also the web origin, as in [`super::relay::pair_link`]. `None` when
/// the relay URL is not an `http(s)://` origin, the uid is zero, or the
/// host id is not 16 lowercase hex — the web would refuse the link anyway.
pub fn guest_link(relay_url: &str, owner_uid: u64, host_id: &str, code: &str) -> Option<String> {
    let origin = relay_url.trim().trim_end_matches('/');
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))?;
    if rest.is_empty() || owner_uid == 0 {
        return None;
    }
    let host_id_ok = host_id.len() == 16
        && host_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !host_id_ok {
        return None;
    }
    Some(format!("{origin}/g/{owner_uid}/{host_id}#invite={code}"))
}

/// Why `canopy remote invite` printed the code without a guest link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoGuestLink {
    /// No host key yet: the host never paired or connected, so it has no
    /// `host_id` for the link to name.
    NoHostKey,
    /// `[remote] relay_url` is not an `http(s)://` origin.
    BadRelayUrl,
}

impl NoGuestLink {
    /// The one-line hint printed after the code instead of a link.
    pub fn hint(self) -> &'static str {
        match self {
            Self::NoHostKey => {
                "No guest link yet: this host has no remote identity. Run `canopy remote on`, \
                 let the daemon connect to the relay, then run `canopy remote invite` again \
                 (`canopy remote uninvite <code>` revokes this code)."
            }
            Self::BadRelayUrl => {
                "No guest link: [remote] relay_url is not an http(s) URL. Send the guest the \
                 code by hand; the guest page asks for it."
            }
        }
    }
}

/// [`guest_link`] for the CLI: the link, or why there is none. `host_id`
/// is `None` when this host has no key yet.
pub fn guest_link_for(
    relay_url: &str,
    owner_uid: u64,
    host_id: Option<&str>,
    code: &str,
) -> std::result::Result<String, NoGuestLink> {
    let Some(host_id) = host_id else {
        return Err(NoGuestLink::NoHostKey);
    };
    guest_link(relay_url, owner_uid, host_id, code).ok_or(NoGuestLink::BadRelayUrl)
}

/// One fresh code: 10 OS-random bytes shown as 16 five-bit Crockford
/// symbols, MSB first, so every code carries exactly 80 bits.
fn generate_code() -> Result<String> {
    let mut bytes = [0u8; CODE_BYTES];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
    let mut symbols = String::with_capacity(CODE_SYMBOLS);
    let mut acc: u32 = 0;
    let mut held: u32 = 0;
    for byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        held += 8;
        while held >= 5 {
            held -= 5;
            symbols.push(ALPHABET[((acc >> held) & 31) as usize] as char);
        }
        acc &= (1 << held) - 1;
    }
    debug_assert!(held == 0, "80 bits split into sixteen 5-bit symbols");
    Ok(format_code(&symbols))
}

/// Render 16 symbols as `CNR-XXXX-XXXX-XXXX-XXXX`.
fn format_code(symbols: &str) -> String {
    format!(
        "CNR-{}-{}-{}-{}",
        &symbols[0..4],
        &symbols[4..8],
        &symbols[8..12],
        &symbols[12..16]
    )
}

#[cfg(test)]
#[path = "invite_tests.rs"]
mod tests;
