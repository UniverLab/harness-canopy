//! The host's long-term P-256 key.
//!
//! Stored as PKCS#8 DER in `<data_dir>/remote/host_key` (mode 0600, parent
//! 0700), created on first need. `host_id` is the first 16 lowercase hex
//! chars of SHA-256 over the public key's SPKI DER — the same derivation
//! browsers use for their own `device_id`. The private key never appears in
//! logs, errors, or status output.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use p256::ecdsa::SigningKey;
use p256::elliptic_curve::Generate as _;
use p256::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use sha2::{Digest as _, Sha256};

/// Path of the host key inside the canopy data directory.
pub fn host_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("remote").join("host_key")
}

/// First 16 lowercase hex chars of SHA-256 over an SPKI DER document.
pub fn id_from_spki(spki: &[u8]) -> String {
    let digest = Sha256::digest(spki);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A loaded host key plus its cached SPKI DER.
pub struct HostKey {
    signing_key: SigningKey,
    spki_der: Vec<u8>,
}

/// ECDSA P-256 SHA-256 signature in IEEE P1363 form (64 bytes `r‖s`).
///
/// [`p256::ecdsa::Signature::to_bytes`] is P1363, never DER, so this is the
/// only encoding the remote protocol ever sees.
pub fn sign_p1363(key: &SigningKey, message: &[u8]) -> [u8; 64] {
    use p256::ecdsa::signature::Signer as _;
    let signature: p256::ecdsa::Signature = key.sign(message);
    signature.to_bytes().into()
}

/// Verify one P-1363 signature against a stored SPKI DER public key.
/// Malformed keys, wrong-length signatures and valid-but-wrong signatures all
/// answer `false`; nothing here distinguishes them for the caller.
pub fn verify_p1363(spki: &[u8], message: &[u8], signature: &[u8]) -> bool {
    use p256::ecdsa::signature::Verifier as _;
    let Ok(key) = p256::ecdsa::VerifyingKey::from_public_key_der(spki) else {
        return false;
    };
    let Ok(signature) = p256::ecdsa::Signature::from_slice(signature) else {
        return false;
    };
    key.verify(message, &signature).is_ok()
}

impl HostKey {
    /// Load the stored key, creating it (and `<data_dir>/remote`) on first
    /// need.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = host_key_path(data_dir);
        let der = match std::fs::read(&path) {
            Ok(der) => der,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let der = generate_pkcs8_der();
                write_private_key(&path, &der)?;
                der
            }
            Err(err) => return Err(err).context("could not read the remote host key"),
        };
        Self::from_pkcs8_der(&der)
    }

    /// Build a key from PKCS#8 DER (tests use the d4eb8558 scalar).
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self> {
        let signing_key = SigningKey::from_pkcs8_der(der)
            .map_err(|_| anyhow::anyhow!("remote host key is not a valid P-256 PKCS#8 key"))?;
        let spki_der = signing_key
            .verifying_key()
            .to_public_key_der()
            .map_err(|_| anyhow::anyhow!("could not encode the remote host public key"))?
            .as_bytes()
            .to_vec();
        Ok(Self {
            signing_key,
            spki_der,
        })
    }

    /// Build a key from the raw 32-byte scalar (test vectors).
    #[cfg(test)]
    pub fn from_scalar_bytes(bytes: &[u8; 32]) -> Result<Self> {
        let signing_key = SigningKey::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("not a valid P-256 scalar"))?;
        let der = signing_key
            .to_pkcs8_der()
            .map_err(|_| anyhow::anyhow!("could not encode the remote host key"))?;
        Self::from_pkcs8_der(der.as_bytes())
    }

    /// The signing key (tests sign with it directly).
    #[allow(dead_code)]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Sign one message; see [`sign_p1363`].
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        sign_p1363(&self.signing_key, message)
    }

    /// The public key's SPKI DER (tests derive device ids from it).
    #[cfg(test)]
    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    /// Standard base64 (padded) of the SPKI DER.
    pub fn spki_b64(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&self.spki_der)
    }

    /// First 16 hex chars of SHA-256(SPKI DER).
    pub fn host_id(&self) -> String {
        id_from_spki(&self.spki_der)
    }
}

fn generate_pkcs8_der() -> Vec<u8> {
    let signing_key = SigningKey::generate();
    signing_key
        .to_pkcs8_der()
        .expect("P-256 keys always encode")
        .as_bytes()
        .to_vec()
}

fn write_private_key(path: &Path, der: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("remote host key path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(der)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d4eb8558_host_key_vector() {
        let key = HostKey::from_scalar_bytes(&[0x11; 32]).expect("valid scalar");
        assert_eq!(key.host_id(), "76805a63a6980c18");
        assert_eq!(
            key.spki_b64(),
            "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEAhfmF/C2RDkoJ4+WmZ5pojpPLBUr321s32bluAKC1O0ZSn3ry5dxLS3aPKhaqHZaVvRfx1hZllLyiXxlMG5XlA=="
        );
    }

    #[test]
    fn load_or_create_writes_a_0600_pkcs8_file_and_reuses_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = HostKey::load_or_create(dir.path()).expect("create");
        let path = host_key_path(dir.path());
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let second = HostKey::load_or_create(dir.path()).expect("reload");
        assert_eq!(first.host_id(), second.host_id());
        assert_eq!(first.spki_b64(), second.spki_b64());
    }

    #[test]
    fn debug_does_not_expose_the_private_key() {
        let key = HostKey::from_scalar_bytes(&[0x11; 32]).expect("valid scalar");
        let rendered = format!("{:?}", key.signing_key());
        assert!(!rendered.contains("1111"));
    }
}
