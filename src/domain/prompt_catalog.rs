//! Loading the [`PromptCatalog`] the hub resolves draft choices against.
//!
//! The sources mirror the TUI pickers exactly: registered projects from the
//! database (the Project picker), `*.md` files under `<canopy_dir>/prompts/`
//! (the Preset picker), and directory skills plus the configured skill store
//! (the Skills picker). Read fresh on every load — no caching — so an
//! external edit shows up immediately, exactly like the pickers.

use std::path::Path;

use crate::db::Database;

use super::prompt_draft::{
    format_project_block, PresetEntry, ProjectEntry, PromptCatalog, SkillEntry,
};

impl PromptCatalog {
    /// Load the full catalog: projects from `db`, presets from
    /// `<canopy_dir>/prompts/`, skills from `workdir`'s `.agents/skills` plus
    /// the global one and the store configured in `canopy_dir`.
    pub fn load(db: &Database, workdir: &Path, canopy_dir: &Path) -> anyhow::Result<PromptCatalog> {
        Ok(PromptCatalog {
            presets: collect_preset_entries(&super::prompts::prompts_dir(canopy_dir)),
            skills: collect_skill_entries(workdir, canopy_dir),
            projects: load_project_entries(db)?,
        })
    }
}

fn load_project_entries(db: &Database) -> anyhow::Result<Vec<ProjectEntry>> {
    let mut entries: Vec<ProjectEntry> = db
        .list_projects()?
        .into_iter()
        .map(|project| ProjectEntry {
            hash: project.hash.clone(),
            name: project.name.clone(),
            path: project.path.clone(),
            block: format_project_block(&project),
        })
        .collect();
    entries.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
    Ok(entries)
}

/// Collect prompt presets: every `*.md` file under `dir` (typically
/// `~/.canopy/prompts/`), sorted by name, with the file's first non-empty
/// line as the preview. Moved from the dialog's
/// `collect_presets_for_picker`, which now delegates here. A missing
/// directory yields an empty list rather than an error; an unreadable file
/// is skipped rather than aborting the whole listing.
pub(crate) fn collect_preset_entries(dir: &Path) -> Vec<PresetEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<PresetEntry> = read_dir
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_stem()?.to_str()?.to_string();
            let content = std::fs::read_to_string(&path).ok()?;
            let preview = content
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .to_string();
            Some(PresetEntry {
                name,
                preview,
                content,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Collect skills from the project directory, the global directory and the
/// configured store, in that order — mirroring the dialog's
/// `collect_skills_for_picker`.
pub(crate) fn collect_skill_entries(workdir: &Path, canopy_dir: &Path) -> Vec<SkillEntry> {
    let mut entries: Vec<SkillEntry> = Vec::new();
    let project = workdir.join(".agents").join("skills");
    scan_skill_dir(&project, &mut entries);
    if let Some(global) = dirs::home_dir().map(|h| h.join(".agents").join("skills")) {
        if global != project {
            scan_skill_dir(&global, &mut entries);
        }
    }
    let config = super::canopy_config::CanopyConfig::load(canopy_dir);
    let store = crate::dynamic_skills::SkillStore::from_config(canopy_dir, &config.skills);
    for skill in store.list_local() {
        entries.push(SkillEntry {
            name: skill.name.clone(),
            label: format!("canopy:{}", skill.name),
        });
    }
    entries
}

/// Add one directory's skills: every subdirectory holding a `SKILL.md` (or
/// `INSTRUCTIONS.md`) contributes its directory name as `skill:<name>`.
fn scan_skill_dir(dir: &Path, out: &mut Vec<SkillEntry>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(raw_name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if crate::skills_module::find_skill_instructions(&path).is_none() {
            continue;
        }
        out.push(SkillEntry {
            name: raw_name.clone(),
            label: format!("skill:{raw_name}"),
        });
    }
}
