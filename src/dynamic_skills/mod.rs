//! Dynamic skill system: skills are fetched on demand from configurable git
//! sources into a canopy-owned store (`~/.canopy/skills/`), kept fresh
//! lazily via a TTL'd commit-hash check, and served to any harness over MCP
//! by `skill_list`/`skill_get`.
//!
//! This is additive to the global-symlink standard in [`crate::skills_module`]
//! — nothing here touches `~/.agents/skills/` or the platform symlinks.

mod catalog_cache;
mod frontmatter;
mod metadata;
mod source;

pub use source::GitSource;
pub use source::SkillFetchError;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use metadata::SkillMetadata;
use source::CatalogEntry;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A skill as reported by `skill_list`: the merged store + catalog view.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillListEntry {
    pub name: String,
    pub description: String,
    pub source_url: String,
    pub installed: bool,
    pub orphaned: bool,
}

/// One reference file belonging to a skill, alongside its instructions.
/// `content` is `None` for files over [`INLINE_FILE_LIMIT`] — use `path`
/// (relative to the skill's store directory) to retrieve them separately.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillFile {
    pub path: String,
    pub content: Option<String>,
}

/// Full skill content as returned by `skill_get`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillContent {
    pub name: String,
    pub instructions: String,
    pub files: Vec<SkillFile>,
    pub source_url: String,
    pub commit_hash: String,
}

/// Reference files at or under this size are inlined in `skill_get`'s
/// response; larger ones are listed by path only.
const INLINE_FILE_LIMIT: u64 = 64 * 1024;

type ClockFn = Box<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Owns the on-disk skill store and the configured sources, and implements
/// the lazy fetch/TTL-refresh dance shared by `skill_list` and `skill_get`.
pub struct SkillStore {
    store_dir: PathBuf,
    sources: Vec<GitSource>,
    ttl: chrono::Duration,
    fetch_lock: Mutex<()>,
    now: ClockFn,
}

impl SkillStore {
    pub fn new(store_dir: PathBuf, sources: Vec<GitSource>, ttl_minutes: u64) -> Self {
        Self {
            store_dir,
            sources,
            ttl: chrono::Duration::minutes(ttl_minutes as i64),
            fetch_lock: Mutex::new(()),
            now: Box::new(Utc::now),
        }
    }

    /// Build a store from the `[skills]` section of `CanopyConfig`, rooted at
    /// `<data_dir>/skills` (i.e. `~/.canopy/skills/`).
    pub fn from_config(
        data_dir: &Path,
        config: &crate::domain::canopy_config::SkillsConfig,
    ) -> Self {
        let sources = config
            .sources
            .iter()
            .map(|s| GitSource::new(s.url.clone(), s.git_ref.clone()))
            .collect();
        Self::new(data_dir.join("skills"), sources, config.ttl_minutes)
    }

    #[cfg(test)]
    fn with_clock(
        store_dir: PathBuf,
        sources: Vec<GitSource>,
        ttl_minutes: u64,
        now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    ) -> Self {
        Self {
            store_dir,
            sources,
            ttl: chrono::Duration::minutes(ttl_minutes as i64),
            fetch_lock: Mutex::new(()),
            now: Box::new(now),
        }
    }

    fn skill_dir(&self, name: &str) -> PathBuf {
        self.store_dir.join(name)
    }

    /// Union of store contents and configured sources' catalogs. Later
    /// sources shadow earlier ones by name. Installed skills always report
    /// the description and source recorded locally, even if a differently
    /// described entry for the same name also exists upstream.
    pub fn list(&self) -> Vec<SkillListEntry> {
        let now = (self.now)();
        let catalogs: Vec<Vec<CatalogEntry>> = self
            .sources
            .iter()
            .map(|source| catalog_cache::catalog_for(&self.store_dir, source, self.ttl, now))
            .collect();
        self.list_with_catalogs(catalogs)
    }

    /// Same merged store-plus-catalog view as [`Self::list`], but built from
    /// the on-disk catalog caches only — never clones, fetches, or refreshes
    /// a source. Used by the Tools picker so listing canopy skills cannot
    /// block the UI on the network. If a source's cache is missing or
    /// unreadable, its catalog entries are simply absent (the store's own
    /// installed skills still appear).
    pub fn list_local(&self) -> Vec<SkillListEntry> {
        let catalogs: Vec<Vec<CatalogEntry>> = self
            .sources
            .iter()
            .map(|source| catalog_cache::cached_catalog(&self.store_dir, source))
            .collect();
        self.list_with_catalogs(catalogs)
    }

    /// Shared merge: fold the per-source catalogs (in order, later shadowing
    /// earlier by name) and then overlay the store's installed skills on
    /// top, marking store skills whose source no longer lists them as
    /// orphaned.
    fn list_with_catalogs(&self, catalogs: Vec<Vec<CatalogEntry>>) -> Vec<SkillListEntry> {
        let mut by_name: std::collections::BTreeMap<String, SkillListEntry> =
            std::collections::BTreeMap::new();
        let mut catalog_names_by_source: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();

        for (source, catalog) in self.sources.iter().zip(catalogs) {
            let names: std::collections::HashSet<String> =
                catalog.iter().map(|e| e.name.clone()).collect();
            for entry in catalog {
                by_name.insert(
                    entry.name.clone(),
                    SkillListEntry {
                        name: entry.name,
                        description: entry.description,
                        source_url: source.url.clone(),
                        installed: false,
                        orphaned: false,
                    },
                );
            }
            catalog_names_by_source.insert(source.url.clone(), names);
        }

        for name in crate::skills_module::list_skill_dirs(&self.store_dir) {
            let dir = self.skill_dir(&name);
            let description = crate::skills_module::find_skill_instructions(&dir)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|c| frontmatter::parse_description(&c))
                .unwrap_or_default();
            let source_url = SkillMetadata::load(&dir)
                .map(|m| m.source_url)
                .unwrap_or_default();
            let orphaned = !source_url.is_empty()
                && catalog_names_by_source
                    .get(&source_url)
                    .map(|set| !set.contains(&name))
                    .unwrap_or(false);
            let description = if orphaned {
                format!("[removed upstream] {description}")
            } else {
                description
            };
            by_name.insert(
                name.clone(),
                SkillListEntry {
                    name,
                    description,
                    source_url,
                    installed: true,
                    orphaned,
                },
            );
        }

        by_name.into_values().collect()
    }

    /// Lazy fetch/update dance, then return the skill's full content:
    /// fetches it if missing, refreshes it if its TTL has expired and the
    /// source's commit hash changed, and always serves the store copy on
    /// network failure.
    pub fn get(&self, name: &str) -> Result<SkillContent> {
        validate_skill_name(name)?;
        let _guard = self.fetch_lock.lock().unwrap();
        let dir = self.skill_dir(name);
        let now = (self.now)();

        match SkillMetadata::load(&dir) {
            Some(meta) => self.refresh_if_stale(name, &dir, meta, now)?,
            None => self.fetch_fresh(name, &dir, now)?,
        }

        self.read_content(name, &dir)
    }

    fn refresh_if_stale(
        &self,
        name: &str,
        dir: &Path,
        meta: SkillMetadata,
        now: DateTime<Utc>,
    ) -> Result<()> {
        if now - meta.last_checked < self.ttl {
            return Ok(());
        }

        let source = GitSource::new(meta.source_url.clone(), meta.git_ref.clone());
        match source.remote_head() {
            Ok(head) if head == meta.commit_hash => SkillMetadata {
                last_checked: now,
                ..meta
            }
            .save(dir),
            Ok(head) => match source.fetch_skill(name, dir) {
                Ok(commit_hash) => SkillMetadata {
                    source_url: meta.source_url,
                    git_ref: meta.git_ref,
                    commit_hash,
                    last_checked: now,
                }
                .save(dir),
                Err(SkillFetchError::NotInSource {
                    name,
                    source_url,
                    head,
                }) => {
                    let short_head = &head[..std::cmp::min(7, head.len())];
                    let catalog =
                        catalog_cache::catalog_for(&self.store_dir, &source, self.ttl, now);
                    let available: Vec<&str> = catalog.iter().map(|e| e.name.as_str()).collect();
                    if let Err(e) = std::fs::remove_dir_all(dir) {
                        tracing::warn!(
                            "dynamic_skills: failed to remove orphan store dir {}: {e}",
                            dir.display()
                        );
                    }
                    anyhow::bail!(
                        "skill '{name}' no longer exists in {source_url} (head {short_head}); it was removed from the local store. Available skills: {}",
                        available.join(", ")
                    )
                }
                Err(SkillFetchError::Other(e)) => {
                    tracing::warn!(
                        "dynamic_skills: update fetch failed for '{name}' \
                         (source head {head} vs stored {}): {e}; serving stale copy",
                        meta.commit_hash
                    );
                    Ok(())
                }
            },
            Err(e) => {
                tracing::warn!(
                    "dynamic_skills: TTL check failed for '{name}' ({e}); \
                     serving stale copy from {}",
                    meta.source_url
                );
                Ok(())
            }
        }
    }

    fn fetch_fresh(&self, name: &str, dir: &Path, now: DateTime<Utc>) -> Result<()> {
        for source in self.sources.iter().rev() {
            match source.fetch_skill(name, dir) {
                Ok(commit_hash) => {
                    return SkillMetadata {
                        source_url: source.url.clone(),
                        git_ref: source.git_ref.clone(),
                        commit_hash,
                        last_checked: now,
                    }
                    .save(dir);
                }
                Err(e) => {
                    tracing::debug!("dynamic_skills: '{name}' not found at {}: {e}", source.url);
                }
            }
        }
        anyhow::bail!("skill '{name}' not found in any configured source")
    }

    fn read_content(&self, name: &str, dir: &Path) -> Result<SkillContent> {
        let instructions_path =
            crate::skills_module::find_skill_instructions(dir).with_context(|| {
                format!("skill '{name}' has no SKILL.md/INSTRUCTIONS.md in the store")
            })?;
        let instructions = std::fs::read_to_string(&instructions_path)?;
        let meta = SkillMetadata::load(dir).context("skill metadata missing after fetch")?;

        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
            let path = entry.path();
            if !path.is_file() || path == instructions_path {
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();
            if rel == metadata::METADATA_FILE {
                continue;
            }
            let content = entry
                .metadata()
                .ok()
                .filter(|m| m.len() <= INLINE_FILE_LIMIT)
                .and_then(|_| std::fs::read_to_string(path).ok());
            files.push(SkillFile { path: rel, content });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(SkillContent {
            name: name.to_string(),
            instructions,
            files,
            source_url: meta.source_url,
            commit_hash: meta.commit_hash,
        })
    }
}

/// Reject anything but a single, plain path component. `name` comes straight
/// from the `skill_get` MCP tool's caller-supplied argument and is joined
/// onto the store dir (and a temp clone dir) to build paths that later get
/// `remove_dir_all`'d and written to — traversal segments like `..` or `/`
/// must never reach that.
fn validate_skill_name(name: &str) -> Result<()> {
    match Path::new(name).components().collect::<Vec<_>>().as_slice() {
        [std::path::Component::Normal(component)] if *component == std::ffi::OsStr::new(name) => {
            Ok(())
        }
        _ => anyhow::bail!("invalid skill name '{name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::TempDir;

    /// Build a local bare-ish git repo (a plain working repo is fine — git
    /// clone/ls-remote work against any local path) with the given skill
    /// directories, each holding a `SKILL.md`. Returns the repo dir.
    fn make_registry(dir: &Path, skills: &[(&str, &str)]) {
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(dir)
            .status()
            .unwrap();
        write_skills(dir, skills);
        commit_all(dir, "init");
    }

    fn write_skills(dir: &Path, skills: &[(&str, &str)]) {
        for (name, description) in skills {
            let skill_dir = dir.join(name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: \"{description}\"\n---\n# {name}\nbody\n"),
            )
            .unwrap();
        }
    }

    fn commit_all(dir: &Path, message: &str) {
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(dir)
            .status()
            .unwrap();
    }

    fn head_sha(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A settable fake clock shared between the test and the store.
    fn fake_clock(
        start: DateTime<Utc>,
    ) -> (Arc<StdMutex<DateTime<Utc>>>, impl Fn() -> DateTime<Utc>) {
        let cell = Arc::new(StdMutex::new(start));
        let reader = Arc::clone(&cell);
        (cell, move || *reader.lock().unwrap())
    }

    #[test]
    fn skill_get_rejects_path_traversal_in_name() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
        );

        for name in ["../escape", "a/b", "..", ".", "", "/etc/passwd"] {
            assert!(
                store.get(name).is_err(),
                "expected '{name}' to be rejected as an invalid skill name"
            );
        }
    }

    #[test]
    fn skill_get_fetches_a_missing_skill_into_the_store() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
        );

        let content = store.get("alpha").unwrap();
        assert_eq!(content.name, "alpha");
        assert!(content.instructions.contains("Alpha skill"));
        assert_eq!(content.commit_hash, head_sha(registry.path()));
        assert!(store_dir.path().join("alpha/SKILL.md").exists());
        assert!(store_dir.path().join("alpha/.canopy-skill.toml").exists());
    }

    #[test]
    fn ttl_is_respected_no_network_check_within_ttl() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();

        // Move the registry away — if a network check happens now, it fails.
        let moved = registry.path().with_extension("moved");
        std::fs::rename(registry.path(), &moved).unwrap();

        // Advance the clock, but stay inside the 15-minute TTL.
        *clock.lock().unwrap() = start + chrono::Duration::minutes(5);

        let content = store.get("alpha").unwrap();
        assert!(content.instructions.contains("Alpha skill"));

        std::fs::rename(&moved, registry.path()).unwrap();
    }

    #[test]
    fn hash_change_past_ttl_triggers_update() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();

        // Update the upstream content and advance past the TTL.
        write_skills(registry.path(), &[("alpha", "Updated alpha skill.")]);
        commit_all(registry.path(), "update alpha");
        *clock.lock().unwrap() = start + chrono::Duration::minutes(20);

        let content = store.get("alpha").unwrap();
        assert!(content.instructions.contains("Updated alpha skill"));
        assert_eq!(content.commit_hash, head_sha(registry.path()));
    }

    #[test]
    fn unchanged_hash_past_ttl_does_not_rewrite_content() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();
        let skill_md = store_dir.path().join("alpha/SKILL.md");
        let mtime_before = std::fs::metadata(&skill_md).unwrap().modified().unwrap();

        // No upstream change, but advance past the TTL so a check happens.
        *clock.lock().unwrap() = start + chrono::Duration::minutes(20);
        std::thread::sleep(std::time::Duration::from_millis(1050));
        store.get("alpha").unwrap();

        let mtime_after = std::fs::metadata(&skill_md).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "SKILL.md must not be rewritten when the source commit hash is unchanged"
        );
    }

    #[test]
    fn network_failure_at_check_time_serves_stale_copy_not_an_error() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();

        // Simulate the source becoming unreachable, then advance past TTL.
        let moved = registry.path().with_extension("moved");
        std::fs::rename(registry.path(), &moved).unwrap();
        *clock.lock().unwrap() = start + chrono::Duration::minutes(20);

        let content = store.get("alpha").expect("stale copy must still be served");
        assert!(content.instructions.contains("Alpha skill"));

        std::fs::rename(&moved, registry.path()).unwrap();
    }

    #[test]
    fn skill_list_merges_store_and_catalog() {
        let registry = TempDir::new().unwrap();
        make_registry(
            registry.path(),
            &[("alpha", "Alpha skill."), ("beta", "Beta skill.")],
        );

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
        );

        // Only fetch "alpha" into the store; "beta" stays catalog-only.
        store.get("alpha").unwrap();

        let entries = store.list();
        let alpha = entries.iter().find(|e| e.name == "alpha").unwrap();
        let beta = entries.iter().find(|e| e.name == "beta").unwrap();
        assert!(alpha.installed);
        assert!(!beta.installed);
        assert_eq!(beta.description, "Beta skill.");
    }

    #[test]
    fn later_source_shadows_earlier_source_by_name() {
        let registry_a = TempDir::new().unwrap();
        make_registry(registry_a.path(), &[("shared", "From source A.")]);
        let registry_b = TempDir::new().unwrap();
        make_registry(registry_b.path(), &[("shared", "From source B.")]);

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![
                GitSource::new(registry_a.path().to_string_lossy().to_string(), None),
                GitSource::new(registry_b.path().to_string_lossy().to_string(), None),
            ],
            15,
        );

        let entries = store.list();
        let shared = entries.iter().find(|e| e.name == "shared").unwrap();
        assert_eq!(shared.description, "From source B.");

        // fetch_fresh should also prefer the later (shadowing) source.
        let content = store.get("shared").unwrap();
        assert_eq!(content.source_url, registry_b.path().to_string_lossy());
    }

    #[test]
    fn skill_removed_upstream_past_ttl_is_purged() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();

        std::fs::remove_dir_all(registry.path().join("alpha")).unwrap();
        commit_all(registry.path(), "remove alpha");
        *clock.lock().unwrap() = start + chrono::Duration::minutes(20);

        let err = store.get("alpha").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no longer exists"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("removed from the local store"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("Available skills:"),
            "unexpected message: {msg}"
        );
        assert!(!store_dir.path().join("alpha").exists());
    }

    #[test]
    fn source_unreachable_past_ttl_keeps_serving_stale_copy() {
        let registry = TempDir::new().unwrap();
        make_registry(registry.path(), &[("alpha", "Alpha skill.")]);

        let store_dir = TempDir::new().unwrap();
        let start = Utc::now();
        let (clock, now_fn) = fake_clock(start);
        let store = SkillStore::with_clock(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
            now_fn,
        );

        store.get("alpha").unwrap();

        let moved = registry.path().with_extension("moved");
        std::fs::rename(registry.path(), &moved).unwrap();
        *clock.lock().unwrap() = start + chrono::Duration::minutes(20);

        let content = store.get("alpha").expect("stale copy must still be served");
        assert!(content.instructions.contains("Alpha skill"));

        std::fs::rename(&moved, registry.path()).unwrap();
    }

    #[test]
    fn list_flags_orphan_and_present() {
        let registry = TempDir::new().unwrap();
        make_registry(
            registry.path(),
            &[("alpha", "Alpha skill."), ("beta", "Beta skill.")],
        );

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
        );

        store.get("alpha").unwrap();
        store.get("beta").unwrap();

        std::fs::remove_dir_all(registry.path().join("alpha")).unwrap();
        commit_all(registry.path(), "remove alpha");

        let entries = store.list();
        let alpha = entries.iter().find(|e| e.name == "alpha").unwrap();
        assert!(alpha.orphaned);
        assert!(
            alpha.description.starts_with("[removed upstream]"),
            "unexpected description: {}",
            alpha.description
        );
        let beta = entries.iter().find(|e| e.name == "beta").unwrap();
        assert!(!beta.orphaned);
    }

    #[test]
    fn list_local_uses_cached_catalogs_when_registry_unreachable() {
        let registry = TempDir::new().unwrap();
        make_registry(
            registry.path(),
            &[("alpha", "Alpha skill."), ("beta", "Beta skill.")],
        );

        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![GitSource::new(
                registry.path().to_string_lossy().to_string(),
                None,
            )],
            15,
        );

        // Prime the cache by calling list() once while registry is reachable.
        let entries_list = store.list();
        assert_eq!(entries_list.len(), 2);
        assert!(entries_list.iter().any(|e| e.name == "alpha"));
        assert!(entries_list.iter().any(|e| e.name == "beta"));

        // Move registry away so network access would fail.
        let moved = registry.path().with_extension("moved");
        std::fs::rename(registry.path(), &moved).unwrap();

        let entries_local = store.list_local();
        assert_eq!(entries_local.len(), 2);
        assert!(entries_local.iter().any(|e| e.name == "alpha"));
        assert!(entries_local.iter().any(|e| e.name == "beta"));

        std::fs::rename(&moved, registry.path()).unwrap();
    }
}
