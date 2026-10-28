//! Pairing-code generation and the `pair_init`/`pair_ok` exchange.
//!
//! The code is 10 Crockford base32 characters from the OS CSPRNG, shown as
//! `XXXXX-XXXXX`, single use, live for ten minutes and burned after five
//! failed MACs. `K_pair` is PBKDF2-HMAC-SHA256 over the normalised code
//! with `canopy-remote/v1/pair\n` + `host_id` as salt, computed once when
//! the code is created and persisted (mode 0600) so the daemon process that
//! receives `pair_init` verifies the same code the CLI printed.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use base64::Engine as _;
use hmac::{Hmac, KeyInit as _, Mac};
use rand::{rngs::SysRng, TryRng as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq as _;

use super::devices::RemoteDevices;
use super::hostkey::HostKey;

/// Crockford base32 alphabet (no `I`, `L`, `O`, `U`).
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Number of characters in a pairing code.
pub const CODE_LEN: usize = 10;
/// Lifetime of a code.
pub const TTL_SECONDS: i64 = 600;
/// Failed MACs after which the code is burned.
pub const MAX_FAILURES: u8 = 5;

const PAIR_SALT_PREFIX: &str = "canopy-remote/v1/pair\n";
const PAIR_INIT_PREFIX: &str = "canopy-remote/v1/pair-init\n";
const PAIR_OK_PREFIX: &str = "canopy-remote/v1/pair-ok\n";

/// A freshly created code, as `canopy remote pair` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedCode {
    /// Formatted as `XXXXX-XXXXX`.
    pub code: String,
    /// Unix seconds when the code stops working.
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCode {
    /// Normalised (uppercase, no separators, `I`/`L`→`1`, `O`→`0`).
    normalized: String,
    /// Standard base64 of K_pair.
    k_pair: String,
    /// Unix seconds when the code was created.
    created_at: i64,
    /// Failed MACs so far.
    fails: u8,
}

/// The one live pairing code, persisted so the CLI and daemon agree.
pub struct PairingState {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PairingState {
    /// State lives at `<data_dir>/remote/pairing.json`.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("remote").join("pairing.json"),
            lock: Mutex::new(()),
        }
    }

    /// Create a new code, replacing any live one. `now` is Unix seconds.
    pub fn create(&self, host_id: &str, now: i64) -> Result<CreatedCode> {
        let code = generate_code()?;
        let normalized = normalize_code(&code);
        let created = StoredCode {
            normalized: normalized.clone(),
            k_pair: b64(&derive_k_pair(&normalized, host_id)),
            created_at: now,
            fails: 0,
        };
        let _guard = self.lock.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        self.write(&created)?;
        Ok(CreatedCode {
            code: format_code(&normalized),
            expires_at: now + TTL_SECONDS,
        })
    }

    /// Answer one `pair_init` body. The reply is always a valid protocol
    /// envelope; the device learns nothing beyond `no_code`, `expired` or
    /// `bad_code`.
    pub fn respond(
        &self,
        body: &Value,
        host: &HostKey,
        devices: &RemoteDevices,
        github_login: &str,
        now: i64,
    ) -> Result<Value> {
        let nonce_h = random_bytes::<16>()?;
        self.respond_with_nonce(body, host, devices, github_login, now, nonce_h)
    }

    /// Deterministic core of [`Self::respond`]; tests pass the vector nonce.
    pub fn respond_with_nonce(
        &self,
        body: &Value,
        host: &HostKey,
        devices: &RemoteDevices,
        github_login: &str,
        now: i64,
        nonce_h: [u8; 16],
    ) -> Result<Value> {
        let _guard = self.lock.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let Some(mut code) = self.read()? else {
            return Ok(pair_fail("no_code"));
        };
        if now >= code.created_at + TTL_SECONDS {
            self.clear()?;
            return Ok(pair_fail("expired"));
        }
        let Some((spki, label, nonce_b)) = decode_init(body) else {
            self.note_failure(&mut code)?;
            return Ok(pair_fail("bad_code"));
        };
        let expected = mac_init(&code.k_pair(), &b64(&spki), &b64(&nonce_b));
        let Some(mac) = body.get("mac").and_then(Value::as_str) else {
            self.note_failure(&mut code)?;
            return Ok(pair_fail("bad_code"));
        };
        let Some(mac) = b64_decode(mac) else {
            self.note_failure(&mut code)?;
            return Ok(pair_fail("bad_code"));
        };
        if !constant_time_eq(&mac, &expected) {
            self.note_failure(&mut code)?;
            return Ok(pair_fail("bad_code"));
        }
        // Success burns the code before anything slow happens.
        self.clear()?;
        devices.pair(spki.clone(), &label, github_login, &paired_at(now))?;
        Ok(pair_ok(
            &code.k_pair(),
            &host.spki_b64(),
            &b64(&spki),
            &b64(&nonce_b),
            &b64(&nonce_h),
        ))
    }

    fn note_failure(&self, code: &mut StoredCode) -> Result<()> {
        code.fails = code.fails.saturating_add(1);
        if code.fails >= MAX_FAILURES {
            self.clear()
        } else {
            self.write(code)
        }
    }

    fn read(&self) -> Result<Option<StoredCode>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text).ok()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).context("could not read the pairing code"),
        }
    }

    fn write(&self, code: &StoredCode) -> Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let dir = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("pairing state has no parent"))?;
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let text = serde_json::to_string(code)?;
        // Write-then-rename (the `settings::writer` shape): a crash between
        // truncate and write must never leave a half-written code behind.
        let temp = self.path.with_extension("json.tmp");
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?
            .write_all(text.as_bytes())?;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&temp, &self.path)?;
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

impl StoredCode {
    fn k_pair(&self) -> [u8; 32] {
        let mut key = [0u8; 32];
        if let Some(bytes) = b64_decode(&self.k_pair) {
            if bytes.len() == 32 {
                key.copy_from_slice(&bytes);
            }
        }
        key
    }
}

/// Uppercase, drop `-` and spaces, `I`/`L`→`1`, `O`→`0`.
pub fn normalize_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

/// Render a normalised code as `XXXXX-XXXXX`.
pub fn format_code(normalized: &str) -> String {
    if normalized.len() <= 5 {
        return normalized.to_string();
    }
    format!("{}-{}", &normalized[..5], &normalized[5..])
}

/// K_pair = PBKDF2-HMAC-SHA256(normalised code, `canopy-remote/v1/pair\n` +
/// host_id, 600000 iterations, 32 bytes).
pub fn derive_k_pair(normalized: &str, host_id: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let salt = format!("{PAIR_SALT_PREFIX}{host_id}");
    pbkdf2::pbkdf2_hmac::<Sha256>(normalized.as_bytes(), salt.as_bytes(), 600_000, &mut out);
    out
}

/// MAC over `pair-init\n` + device SPKI + `\n` + nonce_b (all base64).
pub fn mac_init(k_pair: &[u8; 32], device_spki_b64: &str, nonce_b_b64: &str) -> [u8; 32] {
    hmac_sha256(
        k_pair,
        &format!("{PAIR_INIT_PREFIX}{device_spki_b64}\n{nonce_b_b64}"),
    )
}

/// MAC over `pair-ok\n` + host SPKI + `\n` + device SPKI + `\n` +
/// nonce_b + `\n` + nonce_h (all base64).
pub fn mac_ok(
    k_pair: &[u8; 32],
    host_spki_b64: &str,
    device_spki_b64: &str,
    nonce_b_b64: &str,
    nonce_h_b64: &str,
) -> [u8; 32] {
    hmac_sha256(
        k_pair,
        &format!(
            "{PAIR_OK_PREFIX}{host_spki_b64}\n{device_spki_b64}\n{nonce_b_b64}\n{nonce_h_b64}"
        ),
    )
}

fn hmac_sha256(key: &[u8; 32], message: &str) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.ct_eq(right).into()
}

fn pair_fail(reason: &str) -> Value {
    json!({"t": "pair_fail", "reason": reason})
}

fn pair_ok(
    k_pair: &[u8; 32],
    host_spki_b64: &str,
    device_spki_b64: &str,
    nonce_b_b64: &str,
    nonce_h_b64: &str,
) -> Value {
    let mac = mac_ok(
        k_pair,
        host_spki_b64,
        device_spki_b64,
        nonce_b_b64,
        nonce_h_b64,
    );
    json!({
        "t": "pair_ok",
        "host_spki": host_spki_b64,
        "nonce_h": nonce_h_b64,
        "mac": b64(&mac),
    })
}

fn decode_init(body: &Value) -> Option<(Vec<u8>, String, Vec<u8>)> {
    let spki = b64_decode(body.get("device_spki")?.as_str()?)?;
    let nonce_b = b64_decode(body.get("nonce_b")?.as_str()?)?;
    let label = body
        .get("device_label")
        .and_then(Value::as_str)
        .unwrap_or("browser")
        .to_string();
    Some((spki, label, nonce_b))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn b64_decode(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    SysRng
        .try_fill_bytes(&mut out)
        .map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
    Ok(out)
}

/// Ten Crockford characters from the OS CSPRNG, rejection-sampled so every
/// symbol is equally likely.
pub fn generate_code() -> Result<String> {
    let mut out = String::with_capacity(CODE_LEN);
    while out.len() < CODE_LEN {
        let mut byte = [0u8; 1];
        SysRng
            .try_fill_bytes(&mut byte)
            .map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
        // 224 = 7 * 32: the largest multiple of 32 below 256.
        if byte[0] < 224 {
            out.push(ALPHABET[(byte[0] % 32) as usize] as char);
        }
    }
    Ok(out)
}

fn paired_at(now: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(now, 0)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;
