//! One-time pin of pre-upgrade defaults for existing installs.
//!
//! The `announcements_enabled` default flipped to off and the STUN fallback
//! moved from a compiled Google constant to the `remote.stun_fallback`
//! setting. A config that was already configured before the upgrade keeps
//! behaving the way it did: the first start after the upgrade writes only
//! the keys that are absent (`announcements_enabled = true`, the legacy
//! Google STUN URL), and never touches a fresh install (setup writes
//! `announcements_enabled` itself) or an unparsable file.

use std::path::Path;

use crate::remote::peer_ice::LEGACY_STUN_URL;

/// Keys pinned to their pre-upgrade value, with that value.
const PINNED: &[(&str, &str)] = &[
    ("announcements_enabled", "true"),
    ("remote.stun_fallback", LEGACY_STUN_URL),
];

/// Pin the pre-upgrade values into an already-configured `config.toml`.
///
/// Skips when the file is missing, unparsable, or not configured yet, and
/// writes only the keys that are absent — so a second run changes nothing.
/// Returns the keys it wrote.
pub fn pin_legacy_defaults(data_dir: &Path) -> Vec<&'static str> {
    let path = data_dir.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    if !is_configured(&doc) {
        return Vec::new();
    }
    let mut pinned = Vec::new();
    let mut edits: Vec<(&str, Option<toml_edit::Value>)> = Vec::new();
    for (key, raw) in PINNED.iter().copied() {
        if crate::domain::settings::writer::read_dotted(&doc, key).is_none() {
            let value = if raw == "true" {
                toml_edit::Value::from(true)
            } else {
                toml_edit::Value::from(raw.to_string())
            };
            edits.push((key, Some(value)));
        }
    }
    if edits.is_empty() {
        return pinned;
    }
    if crate::domain::settings::writer::write_keys(data_dir, &edits).is_err() {
        return Vec::new();
    }
    for (key, _) in edits.iter() {
        tracing::info!("pinned {key} to its pre-upgrade value");
        pinned.push(*key);
    }
    pinned
}

/// Whether `config.toml` already recorded a setup run.
fn is_configured(doc: &toml_edit::DocumentMut) -> bool {
    crate::domain::settings::writer::read_dotted(doc, "configured_at")
        .and_then(|value| value.as_str().map(str::to_string))
        .is_some_and(|at| !at.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join(".canopy")
    }

    fn write_config(dir: &tempfile::TempDir, text: &str) {
        let canopy_dir = data_dir(dir);
        std::fs::create_dir_all(&canopy_dir).unwrap();
        std::fs::write(canopy_dir.join("config.toml"), text).unwrap();
    }

    fn read_config(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(data_dir(dir).join("config.toml")).unwrap()
    }

    #[test]
    fn pins_both_keys_once_then_writes_nothing() {
        let dir = tempfile::TempDir::new().unwrap();
        write_config(&dir, "configured_at = \"2026-01-01T00:00:00Z\"\n");

        let first = pin_legacy_defaults(&data_dir(&dir));
        assert_eq!(first, vec!["announcements_enabled", "remote.stun_fallback"]);
        let after = read_config(&dir);
        assert!(after.contains("announcements_enabled = true"), "{after}");
        assert!(after.contains(LEGACY_STUN_URL), "{after}");

        let second = pin_legacy_defaults(&data_dir(&dir));
        assert!(second.is_empty(), "a second run changes nothing");
        assert_eq!(read_config(&dir), after);
    }

    #[test]
    fn skips_missing_unparsable_and_unconfigured_configs() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(pin_legacy_defaults(&data_dir(&dir)).is_empty());

        write_config(&dir, "this is not = = toml");
        assert!(pin_legacy_defaults(&data_dir(&dir)).is_empty());
        assert_eq!(read_config(&dir), "this is not = = toml");

        write_config(&dir, "theme = \"modern\"\n");
        assert!(pin_legacy_defaults(&data_dir(&dir)).is_empty());
        assert!(!read_config(&dir).contains("announcements_enabled"));
    }

    #[test]
    fn keeps_an_explicit_false_and_pins_only_the_missing_key() {
        let dir = tempfile::TempDir::new().unwrap();
        write_config(
            &dir,
            "configured_at = \"2026-01-01T00:00:00Z\"\nannouncements_enabled = false\n",
        );

        let pinned = pin_legacy_defaults(&data_dir(&dir));
        assert_eq!(pinned, vec!["remote.stun_fallback"]);
        let after = read_config(&dir);
        assert!(after.contains("announcements_enabled = false"), "{after}");
    }
}
