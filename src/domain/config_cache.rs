//! Last-valid config cache: a config typo never silently disables remote.
//!
//! [`load`] is the single read path behind [`CanopyConfig::load`]. When
//! `config.toml` fails to parse, the daemon logs the failure and keeps
//! serving the last config that parsed (including `remote.enabled`) until a
//! valid file appears. The fault is recorded in `config.state.json` next to
//! `config.toml` so `remote status` can surface it from another process via
//! [`read_fault_message`]. This module never writes `config.toml` itself.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::canopy_config::CanopyConfig;
use super::remote_config::RemoteConfig;
use super::settings;

/// Sidecar written next to `config.toml` while it fails to parse; removed
/// again once a valid file is loaded.
const STATE_FILE_NAME: &str = "config.state.json";

/// Last-valid `[remote]` table, rewritten on every valid load so a fresh
/// `remote status` process can report the effective values while the file
/// is broken (its in-memory cache starts empty).
const LAST_VALID_FILE_NAME: &str = "config.last-valid.json";

/// One cached canopy dir: the last config that parsed, plus fault state.
struct Entry {
    /// The last file content that parsed, WITHOUT env overlays or the legacy
    /// migration — those are applied per return so they stay live.
    config: CanopyConfig,
    /// RFC 3339 instant the file last parsed; `None` when it never did.
    last_valid_at: Option<String>,
    /// The parse failure currently in effect, if any.
    fault: Option<String>,
}

/// Process-wide cache keyed by canopy dir (tests use distinct `TempDir`s).
fn cache() -> &'static Mutex<HashMap<PathBuf, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Entry>> {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What `config.state.json` stores while the file is invalid.
#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    last_valid_at: Option<String>,
    reason: String,
}

/// Load the config for `canopy_dir`, keeping the last valid one across
/// unreadable or unparsable files. A missing file on a never-seen dir (fresh
/// install) yields the default silently; a parse failure is logged once, on
/// the transition into the faulted state.
pub fn load(canopy_dir: &Path) -> CanopyConfig {
    let config_path = canopy_dir.join("config.toml");
    let parsed = std::fs::read_to_string(&config_path)
        .ok()
        .map(|text| toml::from_str::<CanopyConfig>(&text));
    let file_config = match parsed {
        None => read_fallback(canopy_dir),
        Some(Ok(config)) => store_valid(canopy_dir, config),
        Some(Err(error)) => store_fault(canopy_dir, &config_path, &error.to_string()),
    };
    with_overlays(file_config)
}

/// The `config invalid, ...` line for `remote status`, from the sidecar the
/// faulted load wrote; `None` when the config is currently valid.
pub fn read_fault_message(canopy_dir: &Path) -> Option<String> {
    let state = read_state_file(canopy_dir)?;
    let reason: String = state
        .reason
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect();
    Some(match state.last_valid_at {
        Some(at) => format!("config invalid, using last valid from {at}; config.toml: {reason}"),
        None => format!("config invalid, no last valid config yet; config.toml: {reason}"),
    })
}

/// Read the on-disk fault record, if it parses.
fn read_state_file(canopy_dir: &Path) -> Option<StateFile> {
    let text = std::fs::read_to_string(canopy_dir.join(STATE_FILE_NAME)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Clear the cache. Test-only: the cache is process-wide.
#[cfg(test)]
pub(crate) fn reset() {
    lock_cache().clear();
}

/// Serialize the cache-reset tests: the cache is process-wide, so two
/// `reset()` calls racing under `cargo test` could wipe each other's setup.
/// (Nextest runs each test in its own process; this is for `cargo test`.)
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The file could not be read (missing, or a mid-save rename gap): serve the
/// cached config, or the default on a dir never seen, silently.
fn read_fallback(canopy_dir: &Path) -> CanopyConfig {
    lock_cache()
        .get(canopy_dir)
        .map_or_else(CanopyConfig::default, |entry| entry.config.clone())
}

/// The file parsed: remember it as the new last-valid config, clearing any
/// fault and its sidecar. The sidecar is removed unconditionally: it may be
/// stale from an earlier faulted run in another process, which leaves no
/// in-memory fault here to key the removal on.
fn store_valid(canopy_dir: &Path, config: CanopyConfig) -> CanopyConfig {
    {
        let mut guard = lock_cache();
        guard.insert(
            canopy_dir.to_path_buf(),
            Entry {
                config: config.clone(),
                last_valid_at: Some(chrono::Utc::now().to_rfc3339()),
                fault: None,
            },
        );
    }
    let _ = std::fs::remove_file(canopy_dir.join(STATE_FILE_NAME));
    let _ = std::fs::write(
        canopy_dir.join(LAST_VALID_FILE_NAME),
        serde_json::to_string(&config.remote).unwrap_or_default(),
    );
    config
}

/// The last-valid `[remote]` table from disk, if it parses. `None` on any
/// failure: a missing or corrupt file must never break status.
pub fn read_last_valid_remote(canopy_dir: &Path) -> Option<RemoteConfig> {
    let text = std::fs::read_to_string(canopy_dir.join(LAST_VALID_FILE_NAME)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The `(enabled, relay_url)` pair `remote status` reports: the last-valid
/// values the daemon is actually serving while the file is faulted, else
/// the loaded pair. Env overlays touch `[scheduler]` only, so the stored
/// remote table equals the effective one.
pub fn effective_status_remote(canopy_dir: &Path, loaded: &CanopyConfig) -> (bool, String) {
    if read_state_file(canopy_dir).is_none() {
        return (loaded.remote.enabled, loaded.remote.relay_url.clone());
    }
    match read_last_valid_remote(canopy_dir) {
        Some(last) => (last.enabled, last.relay_url),
        None => (loaded.remote.enabled, loaded.remote.relay_url.clone()),
    }
}

/// The file failed to parse: keep serving the cached (or default) config.
/// The error is reported once, on the transition into the faulted state, so
/// the 1 s re-read cannot spam while the file stays broken.
fn store_fault(canopy_dir: &Path, config_path: &Path, reason: &str) -> CanopyConfig {
    let (fallback, transition) = {
        let mut guard = lock_cache();
        let entry = guard
            .entry(canopy_dir.to_path_buf())
            .or_insert_with(|| Entry {
                config: CanopyConfig::default(),
                last_valid_at: None,
                fault: None,
            });
        let fallback = entry.config.clone();
        let transition = if entry.fault.is_none() {
            entry.fault = Some(reason.to_string());
            Some(StateFile {
                last_valid_at: entry.last_valid_at.clone(),
                reason: reason.to_string(),
            })
        } else {
            None
        };
        (fallback, transition)
    };
    if let Some(mut state) = transition {
        // Another process (the daemon) may have recorded a last-valid
        // instant this process never saw: keep its timestamp rather than
        // downgrading the status line to "no last valid config yet".
        if state.last_valid_at.is_none() {
            if let Some(existing) = read_state_file(canopy_dir) {
                state.last_valid_at = existing.last_valid_at;
            }
        }
        eprintln!(
            "config: {} invalid: {}; keeping last valid",
            config_path.display(),
            reason
        );
        let _ = std::fs::write(
            canopy_dir.join(STATE_FILE_NAME),
            serde_json::to_string_pretty(&state).unwrap_or_default(),
        );
    }
    fallback
}

/// Per-return overlays on a stored or default file config: env first, then
/// the legacy migration.
fn with_overlays(mut config: CanopyConfig) -> CanopyConfig {
    // CM51 (D1): the `CANOPY_RETRY_*` variables overlay the `[scheduler]`
    // table. Injected as a closure so tests never touch the process
    // environment.
    settings::apply_env_overrides(&mut config, &|key| std::env::var(key).ok());
    // Migrate legacy single-root field to the new multi-dir vec.
    if config.rag_personal_dirs.is_empty() && !config.rag_personal_root.is_empty() {
        config
            .rag_personal_dirs
            .push(config.rag_personal_root.clone());
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn canopy_dir(dir: &TempDir) -> PathBuf {
        let path = dir.path().join(".canopy");
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_config(canopy_dir: &Path, content: &str) {
        std::fs::write(canopy_dir.join("config.toml"), content).unwrap();
    }

    #[test]
    fn corrupt_after_valid_keeps_last_valid_config() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        write_config(&canopy_dir, "[remote]\nenabled = true\n");
        assert!(load(&canopy_dir).remote.enabled);

        write_config(&canopy_dir, "theme = \"classic\n");
        let kept = load(&canopy_dir);
        assert!(
            kept.remote.enabled,
            "a corrupt file must not silently disable remote"
        );
        let fault = read_fault_message(&canopy_dir).expect("a fault message while invalid");
        assert!(
            fault.starts_with("config invalid, using last valid from "),
            "{fault}"
        );
        let sidecar = std::fs::read_to_string(canopy_dir.join(STATE_FILE_NAME)).unwrap();
        assert!(sidecar.contains("line"), "{sidecar}");
    }

    #[test]
    fn fixed_file_applies_new_values_and_clears_fault() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        write_config(&canopy_dir, "[remote]\nenabled = true\n");
        assert!(load(&canopy_dir).remote.enabled);
        write_config(&canopy_dir, "theme = \"classic\n");
        assert!(load(&canopy_dir).remote.enabled);
        assert!(canopy_dir.join(STATE_FILE_NAME).exists());

        write_config(
            &canopy_dir,
            "[remote]\nenabled = false\nrelay_url = \"https://example.invalid\"\n",
        );
        let fixed = load(&canopy_dir);
        assert!(!fixed.remote.enabled);
        assert_eq!(fixed.remote.relay_url, "https://example.invalid");
        assert_eq!(read_fault_message(&canopy_dir), None);
        assert!(!canopy_dir.join(STATE_FILE_NAME).exists());
    }

    #[test]
    fn missing_file_returns_default_without_sidecar() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        let config = load(&canopy_dir);
        assert!(!config.remote.enabled);
        assert_eq!(config.remote.relay_url, "https://remote.univerlab.org");
        assert_eq!(read_fault_message(&canopy_dir), None);
        assert!(!canopy_dir.join(STATE_FILE_NAME).exists());
    }

    #[test]
    fn deleted_file_after_valid_keeps_cached_config() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        write_config(&canopy_dir, "[remote]\nenabled = true\n");
        assert!(load(&canopy_dir).remote.enabled);

        std::fs::remove_file(canopy_dir.join("config.toml")).unwrap();
        let kept = load(&canopy_dir);
        assert!(
            kept.remote.enabled,
            "a mid-save rename gap must not disable remote"
        );
        assert_eq!(read_fault_message(&canopy_dir), None);
        assert!(!canopy_dir.join(STATE_FILE_NAME).exists());
    }

    #[test]
    fn valid_load_clears_stale_sidecar_from_another_process() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        write_config(&canopy_dir, "[remote]\nenabled = true\n");
        // A sidecar left by an earlier faulted run: this process holds no
        // in-memory fault, but the file is valid now.
        std::fs::write(
            canopy_dir.join(STATE_FILE_NAME),
            r#"{"last_valid_at":"2026-10-31T12:00:00+00:00","reason":"boom"}"#,
        )
        .unwrap();

        assert!(load(&canopy_dir).remote.enabled);
        assert_eq!(read_fault_message(&canopy_dir), None);
        assert!(!canopy_dir.join(STATE_FILE_NAME).exists());
    }

    #[test]
    fn fault_keeps_sidecar_timestamp_from_another_process() {
        let _guard = test_lock();
        reset();
        let dir = TempDir::new().unwrap();
        let canopy_dir = canopy_dir(&dir);
        write_config(&canopy_dir, "theme = \"classic\n");
        // The daemon faulted earlier with a last-valid instant this fresh
        // process never saw: it must survive this process's own fault.
        std::fs::write(
            canopy_dir.join(STATE_FILE_NAME),
            r#"{"last_valid_at":"2026-10-31T12:00:00+00:00","reason":"old"}"#,
        )
        .unwrap();

        assert!(!load(&canopy_dir).remote.enabled);
        assert_eq!(
            read_fault_message(&canopy_dir).as_deref(),
            Some("config invalid, using last valid from 2026-10-31T12:00:00+00:00; config.toml: TOML parse error at line 1, column 17")
        );
    }
}
