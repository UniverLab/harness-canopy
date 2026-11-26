//! Global skill standard — `~/.agents/skills/` management.
//!
//! Skills live in a single master directory (`~/.agents/skills/`).
//! Canopy creates symlinks from each platform's own skills folder to the
//! master directory so every agent always sees the same set of skills.
//!
//! Layout:
//! ```text
//! ~/.agents/skills/
//!   code-review/
//!     SKILL.md ← instructions injected by the @ picker
//!   rust-idiomatic-patterns/
//!     SKILL.md
//! ~/.kiro/skills/code-review → ~/.agents/skills/code-review (symlink)
//! ```

mod canopy_skill;
pub(crate) mod cleanup;
mod sync_policy;
mod wizard;

use std::path::{Path, PathBuf};

pub use canopy_skill::{install_for_home, rendered};
#[allow(unused_imports)]
pub use wizard::run_skills_wizard;

/// Validate symlink integrity: return broken symlink paths.
/// Used by the `canopy skills` wizard (future subcommand).
#[allow(dead_code)]
pub fn find_broken_symlinks(
    home: &Path,
    platforms: &[&crate::setup_module::Platform],
) -> Vec<PathBuf> {
    platform_skill_dirs(home, platforms)
        .flat_map(|(_, platform_skills)| broken_symlinks_in_dir(&platform_skills))
        .collect()
}

fn broken_symlinks_in_dir(dir: &Path) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_symlink() && !path.exists())
        .collect()
}

/// List immediate subdirectory names inside `dir` that look like skill folders
/// (contain a `SKILL.md` or `INSTRUCTIONS.md`).
pub fn list_skill_dirs(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| contains_skill_instructions(&e.path()))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    names.sort();
    names
}

/// Locate the instructions file for a skill directory.
/// Returns `Some(path)` for `SKILL.md`, then `INSTRUCTIONS.md`, otherwise `None`.
pub fn find_skill_instructions(skill_dir: &Path) -> Option<PathBuf> {
    ["SKILL.md", "INSTRUCTIONS.md"]
        .into_iter()
        .map(|name| skill_dir.join(name))
        .find(|path| path.exists())
}

// ── Utilities (shared with submodules) ────────────────────────────────────

pub(super) fn global_skills_dir_for(home: &Path) -> PathBuf {
    home.join(".agents").join("skills")
}

fn contains_skill_instructions(dir: &Path) -> bool {
    find_skill_instructions(dir).is_some()
}

pub(super) fn platform_skill_dirs<'a>(
    home: &'a Path,
    platforms: &'a [&'a crate::setup_module::Platform],
) -> impl Iterator<Item = (&'a crate::setup_module::Platform, PathBuf)> + 'a {
    platforms.iter().copied().filter_map(move |platform| {
        platform_skill_dir(home, platform).map(|skills_dir| (platform, skills_dir))
    })
}

fn platform_skill_dir(home: &Path, platform: &crate::setup_module::Platform) -> Option<PathBuf> {
    let skills_dir = platform.skills_dir.as_deref()?;
    Some(home.join(skills_dir))
}
