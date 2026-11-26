//! Cleanup of skill copies earlier canopy versions installed.
//!
//! `canopy setup` used to download the Essential Pack into
//! `~/.agents/skills/` and symlink each skill into every harness
//! `skills_dir`, and the old MCP store kept fetched skills under
//! `~/.canopy/skills/`. Those copies go stale, so setup and
//! `canopy skill install` now remove them — deleting only paths the rules
//! below prove canopy installed, and keeping everything else.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::sync_policy::{decide_sync_action, SyncAction};
use crate::setup_module::Platform;

const ESSENTIAL_PACK_API: &str = "https://api.github.com/repos/UniverLab/skills/contents";

/// One entry of the GitHub `contents` API, as `fetch_essential_pack_entries`
/// used to read it before the download step was removed.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct GhEntry {
    pub(crate) name: String,
    #[serde(rename = "type")]
    pub(crate) entry_type: String,
    pub(crate) download_url: Option<String>,
}

/// Source of the Essential Pack catalog. Production reads GitHub; tests
/// inject a fixture so no test ever touches the network.
pub(crate) trait CatalogSource {
    /// Top-level directory listing, or `None` when unreachable.
    fn fetch_top(&self) -> Option<Vec<GhEntry>>;
    /// File listing of one skill directory, or `None` when unreachable.
    fn fetch_skill(&self, name: &str) -> Option<Vec<GhEntry>>;
    /// Raw bytes of one upstream file, or `None` when unreachable.
    fn fetch_bytes(&self, url: &str) -> Option<Vec<u8>>;
}

/// [`CatalogSource`] backed by the GitHub API.
pub(crate) struct GithubCatalogSource {
    client: reqwest::blocking::Client,
}

impl GithubCatalogSource {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent("canopy")
            .build()?;
        Ok(Self { client })
    }
}

impl CatalogSource for GithubCatalogSource {
    fn fetch_top(&self) -> Option<Vec<GhEntry>> {
        let response = self.client.get(ESSENTIAL_PACK_API).send().ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().ok()
    }

    fn fetch_skill(&self, name: &str) -> Option<Vec<GhEntry>> {
        let url = format!("{ESSENTIAL_PACK_API}/{name}");
        let response = self.client.get(&url).send().ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().ok()
    }

    fn fetch_bytes(&self, url: &str) -> Option<Vec<u8>> {
        let response = self.client.get(url).send().ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.bytes().ok().map(|body| body.to_vec())
    }
}

/// Remove every legacy skill copy and report one line per area plus one
/// `kept:` line per directory deliberately left in place. Idempotent: once
/// everything removable is gone, every count prints zero.
pub(crate) fn run_skills_cleanup(
    home: &Path,
    platforms: &[Platform],
    force: bool,
    source: &dyn CatalogSource,
) -> Vec<String> {
    let mut lines = Vec::new();
    let global = super::global_skills_dir_for(home);
    let removed_links = cleanup_harness_symlinks(home, platforms, &global);
    lines.push(format!("skills cleanup: removed {removed_links} symlinks"));
    match source.fetch_top() {
        None => {
            lines.push("skills cleanup: catalog unreachable, kept ~/.agents/skills".to_string());
        }
        Some(top) => {
            let (removed, kept) = cleanup_pack_copies(&global, &top, force, source, &mut lines);
            lines.push(format!(
                "skills cleanup: removed {removed} skill copies, kept {kept}"
            ));
        }
    }
    let removed_store = cleanup_old_store(home);
    lines.push(format!(
        "skills cleanup: removed {removed_store} old store skills"
    ));
    lines
}

/// Step (a): remove per-skill symlinks pointing into `~/.agents/skills/`.
/// Regular files, directories, symlinks elsewhere, and the `canopy` skill
/// itself are never touched.
fn cleanup_harness_symlinks(home: &Path, platforms: &[Platform], global: &Path) -> usize {
    let mut removed = 0;
    let mut seen = HashSet::new();
    for platform in platforms {
        let Some(relative) = platform.skills_dir.as_deref() else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(home.join(relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "canopy") {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            if is_proven_canopy_symlink(&path, global) && std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// A symlink counts as canopy-installed iff its target sits inside the
/// global skills directory the old `create_platform_symlinks` linked from.
fn is_proven_canopy_symlink(path: &Path, global: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    std::fs::read_link(path).is_ok_and(|target| target.starts_with(global))
}

/// Step (b): remove `~/.agents/skills/` directories that still match the
/// upstream pack. Returns `(removed, kept)`, appending one `kept:` line per
/// directory left in place.
fn cleanup_pack_copies(
    global: &Path,
    top: &[GhEntry],
    force: bool,
    source: &dyn CatalogSource,
    lines: &mut Vec<String>,
) -> (usize, usize) {
    let catalog: HashSet<&str> = top
        .iter()
        .filter(|entry| entry.entry_type == "dir")
        .map(|entry| entry.name.as_str())
        .collect();
    let Ok(entries) = std::fs::read_dir(global) else {
        return (0, 0);
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    let mut removed = 0;
    let mut kept = 0;
    for name in names {
        let dir = global.join(&name);
        if !catalog.contains(name.as_str()) {
            lines.push(format!("kept: {name} (not from canopy)"));
            kept += 1;
            continue;
        }
        let verdict = pack_verdict(&name, &dir, source);
        if verdict == PackVerdict::Match || (force && verdict == PackVerdict::Diverged) {
            if std::fs::remove_dir_all(&dir).is_ok() {
                removed += 1;
            }
        } else if verdict == PackVerdict::Diverged {
            lines.push(format!("kept: {name} (edited locally)"));
            kept += 1;
        } else {
            lines.push(format!("kept: {name} (upstream unreachable)"));
            kept += 1;
        }
    }
    (removed, kept)
}

/// Outcome of comparing one local skill directory against upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackVerdict {
    /// Byte-identical to upstream (ignoring `.sync-new` sidecars): remove.
    Match,
    /// Local content diverges: keep unless forced.
    Diverged,
    /// Upstream listing or a file fetch failed: keep even when forced.
    Unreachable,
}

fn pack_verdict(name: &str, dir: &Path, source: &dyn CatalogSource) -> PackVerdict {
    let Some(entries) = source.fetch_skill(name) else {
        return PackVerdict::Unreachable;
    };
    match local_matches_upstream(dir, &entries, source) {
        None => PackVerdict::Unreachable,
        Some(true) => PackVerdict::Match,
        Some(false) => PackVerdict::Diverged,
    }
}

/// Fail-closed three-way comparison: `Some(true)` iff the directory holds
/// no subdirectories, no extra non-sidecar files, and every local file is
/// byte-identical (SHA-256 via `sync_policy`) to its upstream counterpart.
/// `None` iff an upstream fetch failed mid-comparison. A local directory
/// missing an upstream file still matches as long as every present file
/// matches — partial installs clean up too.
fn local_matches_upstream(
    dir: &Path,
    entries: &[GhEntry],
    source: &dyn CatalogSource,
) -> Option<bool> {
    let upstream: HashMap<&str, &GhEntry> = entries
        .iter()
        .filter(|entry| entry.entry_type == "file")
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    let Ok(local) = std::fs::read_dir(dir) else {
        return Some(false);
    };
    for entry in local.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            return Some(false);
        }
        if name.ends_with(".sync-new") {
            continue;
        }
        let Some(remote) = upstream.get(name.as_str()) else {
            return Some(false);
        };
        let url = remote.download_url.as_deref()?;
        let Ok(local_bytes) = std::fs::read(entry.path()) else {
            return Some(false);
        };
        let remote_bytes = source.fetch_bytes(url)?;
        if decide_sync_action(Some(&local_bytes), &remote_bytes, false) == SyncAction::Skip {
            return Some(false);
        }
    }
    Some(true)
}

/// Step (c): remove old MCP store skill directories (the ones holding the
/// `.canopy-skill.toml` metadata file), then remove the store directory
/// itself iff nothing is left in it.
fn cleanup_old_store(home: &Path) -> usize {
    let store = home.join(".canopy").join("skills");
    let Ok(entries) = std::fs::read_dir(&store) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if !entry
            .path()
            .join(crate::dynamic_skills::METADATA_FILE)
            .is_file()
        {
            continue;
        }
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    if store.read_dir().is_ok_and(|mut rest| rest.next().is_none()) {
        let _ = std::fs::remove_dir(&store);
    }
    removed
}

/// Fixture [`CatalogSource`] for tests: no network, fully scripted.
#[cfg(test)]
pub(crate) struct FixtureCatalog {
    pub(crate) top: Option<Vec<GhEntry>>,
    pub(crate) skills: HashMap<String, Option<Vec<GhEntry>>>,
    pub(crate) bytes: HashMap<String, Option<Vec<u8>>>,
}

#[cfg(test)]
impl FixtureCatalog {
    pub(crate) fn dir_entry(name: &str) -> GhEntry {
        GhEntry {
            name: name.to_string(),
            entry_type: "dir".to_string(),
            download_url: None,
        }
    }

    pub(crate) fn file_entry(name: &str, url: &str) -> GhEntry {
        GhEntry {
            name: name.to_string(),
            entry_type: "file".to_string(),
            download_url: Some(url.to_string()),
        }
    }

    pub(crate) fn with_skill(
        top: Vec<GhEntry>,
        skill: &str,
        entries: Vec<GhEntry>,
        files: Vec<(&str, &[u8])>,
    ) -> Self {
        Self {
            top: Some(top),
            skills: HashMap::from([(skill.to_string(), Some(entries))]),
            bytes: files
                .into_iter()
                .map(|(url, body)| (url.to_string(), Some(body.to_vec())))
                .collect(),
        }
    }
}

#[cfg(test)]
impl CatalogSource for FixtureCatalog {
    fn fetch_top(&self) -> Option<Vec<GhEntry>> {
        self.top.clone()
    }

    fn fetch_skill(&self, name: &str) -> Option<Vec<GhEntry>> {
        self.skills.get(name).cloned().flatten()
    }

    fn fetch_bytes(&self, url: &str) -> Option<Vec<u8>> {
        self.bytes.get(url).cloned().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SKILL_MD: &[u8] = b"# skill\n";
    const SKILL_URL: &str = "https://raw/skills/alpha/SKILL.md";

    fn platform(name: &str, skills_dir: &str) -> Platform {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "config_path": ".config/test",
            "mcp_servers_key": [],
            "skills_dir": skills_dir,
        }))
        .unwrap()
    }

    fn platforms() -> Vec<Platform> {
        vec![platform("alpha", "alpha/.skills")]
    }

    fn fixture_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn pack_fixture() -> FixtureCatalog {
        FixtureCatalog::with_skill(
            vec![FixtureCatalog::dir_entry("alpha")],
            "alpha",
            vec![FixtureCatalog::file_entry("SKILL.md", SKILL_URL)],
            vec![(SKILL_URL, SKILL_MD)],
        )
    }

    fn write_pack_dir(global: &Path, content: &[u8]) {
        let dir = global.join("alpha");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    #[test]
    fn removes_symlink_into_global_skills_and_keeps_the_rest() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        std::fs::create_dir_all(global.join("alpha")).unwrap();
        let harness = home.join("alpha/.skills");
        std::fs::create_dir_all(&harness).unwrap();
        std::os::unix::fs::symlink(global.join("alpha"), harness.join("alpha")).unwrap();
        std::os::unix::fs::symlink("/elsewhere", harness.join("other")).unwrap();
        std::fs::create_dir_all(harness.join("real")).unwrap();

        let source = FixtureCatalog {
            top: Some(vec![]),
            skills: HashMap::new(),
            bytes: HashMap::new(),
        };
        let lines = run_skills_cleanup(home, &platforms(), false, &source);

        assert!(!harness.join("alpha").is_symlink());
        assert!(harness.join("other").is_symlink());
        assert!(harness.join("real").is_dir());
        assert!(lines
            .iter()
            .any(|line| line == "skills cleanup: removed 1 symlinks"));
    }

    #[test]
    fn unedited_pack_directory_is_removed_with_its_sidecar() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, SKILL_MD);
        std::fs::write(global.join("alpha").join("SKILL.md.sync-new"), b"stale").unwrap();

        let lines = run_skills_cleanup(home, &platforms(), false, &pack_fixture());

        assert!(!global.join("alpha").exists());
        assert!(lines
            .iter()
            .any(|line| line == "skills cleanup: removed 1 skill copies, kept 0"));
    }

    #[test]
    fn edited_pack_directory_is_kept_and_reported_then_removed_with_force() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, b"locally edited");

        let first = run_skills_cleanup(home, &platforms(), false, &pack_fixture());

        assert!(global.join("alpha/SKILL.md").is_file());
        assert!(first
            .iter()
            .any(|line| line == "kept: alpha (edited locally)"));

        let forced = run_skills_cleanup(home, &platforms(), true, &pack_fixture());

        assert!(!global.join("alpha").exists());
        assert!(forced
            .iter()
            .any(|line| line == "skills cleanup: removed 1 skill copies, kept 0"));
    }

    #[test]
    fn extra_file_and_subdirectory_count_as_edited() {
        for extra in ["extra", "subdir"] {
            let fixture = fixture_home();
            let home = fixture.path();
            let global = home.join(".agents/skills");
            write_pack_dir(&global, SKILL_MD);
            let extra_path = global.join("alpha").join(extra);
            if extra == "subdir" {
                std::fs::create_dir_all(&extra_path).unwrap();
            } else {
                std::fs::write(&extra_path, b"x").unwrap();
            }

            let lines = run_skills_cleanup(home, &platforms(), false, &pack_fixture());

            assert!(global.join("alpha").exists(), "kept with {extra}");
            assert!(lines
                .iter()
                .any(|line| line == "kept: alpha (edited locally)"));
        }
    }

    #[test]
    fn unknown_directory_is_kept_and_reported() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        std::fs::create_dir_all(global.join("mine")).unwrap();
        std::fs::write(global.join("mine/SKILL.md"), b"mine").unwrap();

        let lines = run_skills_cleanup(home, &platforms(), false, &pack_fixture());

        assert!(global.join("mine").is_dir());
        assert!(lines
            .iter()
            .any(|line| line == "kept: mine (not from canopy)"));
    }

    #[test]
    fn unreachable_skill_fetch_is_kept_even_with_force() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, SKILL_MD);
        let source = FixtureCatalog {
            top: Some(vec![FixtureCatalog::dir_entry("alpha")]),
            skills: HashMap::new(),
            bytes: HashMap::new(),
        };

        let lines = run_skills_cleanup(home, &platforms(), true, &source);

        assert!(global.join("alpha").is_dir());
        assert!(lines
            .iter()
            .any(|line| line == "kept: alpha (upstream unreachable)"));
    }

    #[test]
    fn unreachable_file_fetch_is_kept_even_with_force() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, SKILL_MD);
        // Listing resolves, but the file bytes never arrive.
        let source = FixtureCatalog {
            top: Some(vec![FixtureCatalog::dir_entry("alpha")]),
            skills: HashMap::from([(
                "alpha".to_string(),
                Some(vec![FixtureCatalog::file_entry("SKILL.md", SKILL_URL)]),
            )]),
            bytes: HashMap::new(),
        };

        let lines = run_skills_cleanup(home, &platforms(), true, &source);

        assert!(global.join("alpha/SKILL.md").is_file());
        assert!(lines
            .iter()
            .any(|line| line == "kept: alpha (upstream unreachable)"));
    }

    #[test]
    fn unreachable_top_level_catalog_skips_only_pack_step() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, SKILL_MD);
        let harness = home.join("alpha/.skills");
        std::fs::create_dir_all(&harness).unwrap();
        std::os::unix::fs::symlink(global.join("alpha"), harness.join("alpha")).unwrap();
        let store_skill = home.join(".canopy/skills/old");
        std::fs::create_dir_all(&store_skill).unwrap();
        std::fs::write(
            store_skill.join(crate::dynamic_skills::METADATA_FILE),
            b"source_url = 'x'",
        )
        .unwrap();
        let source = FixtureCatalog {
            top: None,
            skills: HashMap::new(),
            bytes: HashMap::new(),
        };

        let lines = run_skills_cleanup(home, &platforms(), false, &source);

        assert!(lines
            .iter()
            .any(|line| line == "skills cleanup: catalog unreachable, kept ~/.agents/skills"));
        assert!(global.join("alpha").is_dir(), "pack step skipped");
        assert!(!store_skill.exists(), "store step still runs");
        assert!(
            !harness.join("alpha").is_symlink(),
            "symlink step still runs"
        );
    }

    #[test]
    fn old_store_skill_directories_are_removed_and_unrelated_kept() {
        let fixture = fixture_home();
        let home = fixture.path();
        let store = home.join(".canopy/skills");
        let old = store.join("old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(crate::dynamic_skills::METADATA_FILE), b"x").unwrap();
        std::fs::create_dir_all(store.join("unrelated")).unwrap();

        let source = FixtureCatalog {
            top: Some(vec![]),
            skills: HashMap::new(),
            bytes: HashMap::new(),
        };
        let lines = run_skills_cleanup(home, &platforms(), false, &source);

        assert!(!old.exists());
        assert!(store.join("unrelated").is_dir());
        assert!(lines
            .iter()
            .any(|line| line == "skills cleanup: removed 1 old store skills"));
    }

    #[test]
    fn second_run_prints_zeros() {
        let fixture = fixture_home();
        let home = fixture.path();
        let global = home.join(".agents/skills");
        write_pack_dir(&global, SKILL_MD);
        let harness = home.join("alpha/.skills");
        std::fs::create_dir_all(&harness).unwrap();
        std::os::unix::fs::symlink(global.join("alpha"), harness.join("alpha")).unwrap();
        let source = pack_fixture();

        run_skills_cleanup(home, &platforms(), false, &source);
        let second = run_skills_cleanup(home, &platforms(), false, &source);

        assert_eq!(
            second,
            vec![
                "skills cleanup: removed 0 symlinks".to_string(),
                "skills cleanup: removed 0 skill copies, kept 0".to_string(),
                "skills cleanup: removed 0 old store skills".to_string(),
            ]
        );
    }
}
