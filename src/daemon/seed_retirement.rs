//! One-time retirement of the legacy `~/.canopy/seeds/` directory.
//!
//! Identities were removed; existing on-disk seed data is preserved by moving
//! it out of the way once. The directory is never read again.

use std::path::Path;

/// Move `<canopy_home>/seeds/` to `<canopy_home>/seeds-retired-<YYYY-MM-DD>/`
/// once. If the target already exists, a numeric suffix is appended. Errors are
/// logged and swallowed so startup is never blocked by a best-effort migration.
pub(crate) fn retire_legacy_seeds_dir(canopy_home: &Path) {
    let seeds = canopy_home.join("seeds");
    if !seeds.is_dir() {
        return;
    }
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let base = canopy_home.join(format!("seeds-retired-{date}"));
    let target = unique_retired_target(&base);
    match std::fs::rename(&seeds, &target) {
        Ok(()) => {
            tracing::info!(
                from = %seeds.display(),
                to = %target.display(),
                "retired legacy seeds directory"
            );
        }
        Err(e) => {
            tracing::warn!(
                from = %seeds.display(),
                to = %target.display(),
                error = %e,
                "failed to retire legacy seeds directory"
            );
        }
    }
}

fn unique_retired_target(base: &Path) -> std::path::PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    for n in 2u32.. {
        let candidate = base.with_file_name(format!(
            "{}-{n}",
            base.file_name().unwrap_or_default().to_string_lossy()
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    base.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn moves_seeds_dir_once_to_dated_retired_target() {
        let home = tempdir().unwrap();
        let seeds = home.path().join("seeds");
        std::fs::create_dir_all(seeds.join("oak")).unwrap();
        std::fs::write(seeds.join("oak/identity.toml"), "name='Oak'").unwrap();

        retire_legacy_seeds_dir(home.path());

        assert!(!seeds.exists(), "seeds dir should be moved away");
        let mut retired: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("seeds-retired-"))
            .collect();
        retired.sort();
        assert_eq!(retired.len(), 1);
        let retired_path = home.path().join(&retired[0]);
        assert!(retired_path.join("oak/identity.toml").exists());
        let content = std::fs::read_to_string(retired_path.join("oak/identity.toml")).unwrap();
        assert_eq!(content, "name='Oak'");

        // Second start: no-op, nothing moved again.
        retire_legacy_seeds_dir(home.path());
        let retired2: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("seeds-retired-"))
            .collect();
        assert_eq!(retired2.len(), 1);
    }

    #[test]
    fn no_seeds_dir_is_noop() {
        let home = tempdir().unwrap();
        retire_legacy_seeds_dir(home.path());
        let entries: Vec<_> = std::fs::read_dir(home.path()).unwrap().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn existing_target_gets_numeric_suffix() {
        let home = tempdir().unwrap();
        let seeds = home.path().join("seeds");
        std::fs::create_dir_all(&seeds).unwrap();
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let base = home.path().join(format!("seeds-retired-{date}"));
        std::fs::create_dir_all(&base).unwrap();

        retire_legacy_seeds_dir(home.path());

        assert!(!seeds.exists());
        let suffixed = home.path().join(format!("seeds-retired-{date}-2"));
        assert!(suffixed.exists());
    }
}
