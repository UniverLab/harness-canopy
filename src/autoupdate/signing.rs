//! Minisign verification of release artifacts.
//!
//! Every signed release publishes `SHA256SUMS.txt` (lines of
//! `<64-hex>  <asset name>`) plus a detached minisign signature of that
//! file, `SHA256SUMS.txt.minisig`, whose trusted comment is exactly
//! `canopy <tag>`. [`verify_release`] checks the signature against the
//! embedded [`PUBLIC_KEY`], binds it to the tag through the trusted
//! comment, and binds the downloaded archive to the checksums file.

use anyhow::{anyhow, bail, Result};
use sha2::Digest as _;

/// Production release signing key (key id `2A96676A759C3943`), embedded so
/// `canopy update` verifies against a key compiled into the binary instead
/// of one fetched alongside the release.
pub const PUBLIC_KEY: &str = "RWRDOZx1ameWKtWCHDVp3YmIsBiJszM2U4SJbCMkZ8/iSkW9Pi1yWzDD";

/// First release carrying `SHA256SUMS.txt.minisig`. Older tags predate
/// signing: below this version a missing signature installs with a warning,
/// at or above it a missing signature aborts.
pub const SIGNED_RELEASES_FROM: &str = "3.3.0";

/// Whether `tag` must carry a signature: true at or above
/// [`SIGNED_RELEASES_FROM`].
pub fn is_signature_mandatory(tag: &str) -> bool {
    super::compare_versions(tag, SIGNED_RELEASES_FROM).is_ge()
}

/// Verify a downloaded release archive against its checksums file.
///
/// `sums` is the `SHA256SUMS.txt` content, `minisig` the
/// `SHA256SUMS.txt.minisig` content, `archive` the raw bytes of `asset`,
/// and `tag` the release tag (for example `v3.3.0`). Each failure names its
/// check first: `signature invalid:`, `trusted comment mismatch:`,
/// `checksum entry missing:`, or `checksum mismatch:`.
pub fn verify_release(
    public_key: &str,
    sums: &str,
    minisig: &str,
    archive: &[u8],
    asset: &str,
    tag: &str,
) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(public_key)
        .map_err(|error| anyhow!("signature invalid: bad public key: {error}"))?;
    let signature = minisign_verify::Signature::decode(minisig.trim())
        .map_err(|error| anyhow!("signature invalid: bad signature: {error}"))?;
    key.verify(sums.as_bytes(), &signature, false)
        .map_err(|error| anyhow!("signature invalid: {error}"))?;
    // `trusted_comment()` strips the `trusted comment: ` prefix, so the
    // comparison is against the bare `canopy <tag>`.
    let expected_comment = format!("canopy {tag}");
    if signature.trusted_comment() != expected_comment {
        bail!(
            "trusted comment mismatch: expected {expected_comment:?}, got {:?}",
            signature.trusted_comment()
        );
    }
    let mut expected_hash = None;
    for line in sums.lines() {
        if let Some((hash, name)) = line.split_once("  ") {
            if name == asset {
                expected_hash = Some(hash);
                break;
            }
        }
    }
    let Some(expected) = expected_hash else {
        bail!("checksum entry missing: {asset} is not listed in SHA256SUMS.txt");
    };
    let actual: String = sha2::Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        bail!("checksum mismatch: {asset} does not match SHA256SUMS.txt");
    }
    Ok(())
}
