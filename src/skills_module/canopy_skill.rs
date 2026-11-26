//! The `canopy` skill embedded in the binary.
//!
//! The skill source is `canopy_skill.md` next to this file, baked into the
//! binary with [`include_str!`] so it is never older than the binary itself.
//! [`install_for_home`] writes the rendered text as a regular file into
//! every configured harness skills directory (`<skills_dir>/canopy/SKILL.md`),
//! refreshing it whenever the binary's version changes. Hand-written files
//! without a `canopy_version` marker are left untouched.

use std::path::Path;

use crate::setup_module::Platform;

/// Rendered skill text with the binary's version stamped in.
pub fn rendered() -> String {
    include_str!("canopy_skill.md").replace("{{VERSION}}", binary_version())
}

/// The binary's own version, stamped into the rendered skill.
pub fn binary_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Outcome of installing the skill into one harness, for the report line.
pub enum InstallStatus {
    /// Freshly written (or a legacy symlink replaced by a regular file).
    Installed,
    /// Rewritten because the on-disk version differed.
    UpdatedFrom(String),
    /// Already at the binary's version.
    Current,
    /// Hand-written file without a `canopy_version` marker; untouched.
    SkippedUnmanaged,
}

/// Write the embedded skill into every platform skills directory that exists
/// under `home`.
///
/// A harness is skipped silently when its skills directory does not exist
/// (it is never created here). With `only_existing` set, harnesses without
/// an existing `canopy/SKILL.md` are skipped too, so a daemon restart only
/// refreshes what a previous install or setup already placed. Returns one
/// report line per harness touched or deliberately left alone.
pub fn install_for_home(home: &Path, platforms: &[Platform], only_existing: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for platform in platforms {
        let Some(skills_dir) = platform.skills_dir.as_deref() else {
            continue;
        };
        if let Some(line) =
            install_for_platform(&platform.name, &home.join(skills_dir), only_existing)
        {
            lines.push(line);
        }
    }
    lines
}

/// Install into one harness skills directory, or `None` to stay silent.
fn install_for_platform(name: &str, dir: &Path, only_existing: bool) -> Option<String> {
    if !dir.exists() {
        return None;
    }
    let target = dir.join("canopy").join("SKILL.md");
    let present = target.exists() || target.is_symlink();
    if only_existing && !present {
        return None;
    }
    if target.is_symlink() {
        // Legacy layout linked this path elsewhere; the embedded skill is
        // always a regular file, so drop the link before writing.
        if let Err(error) = std::fs::remove_file(&target) {
            tracing::warn!(
                "canopy skill: cannot replace symlink {}: {error}",
                target.display()
            );
            return None;
        }
        return write_fresh(name, &target, InstallStatus::Installed);
    }
    if !present {
        return write_fresh(name, &target, InstallStatus::Installed);
    }
    match std::fs::read_to_string(&target) {
        Ok(content) => match skill_version(&content) {
            None => write_status(name, InstallStatus::SkippedUnmanaged, &target),
            Some(old) if old == binary_version() => {
                write_status(name, InstallStatus::Current, &target)
            }
            Some(old) => write_fresh(name, &target, InstallStatus::UpdatedFrom(old)),
        },
        Err(error) => {
            tracing::warn!("canopy skill: cannot read {}: {error}", target.display());
            None
        }
    }
}

/// Write the rendered skill and report the outcome. The target is always a
/// regular file: callers remove legacy symlinks before reaching this.
fn write_fresh(name: &str, target: &Path, status: InstallStatus) -> Option<String> {
    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            tracing::warn!("canopy skill: cannot create {}: {error}", parent.display());
            return None;
        }
    }
    if let Err(error) = std::fs::write(target, rendered()) {
        tracing::warn!("canopy skill: cannot write {}: {error}", target.display());
        return None;
    }
    write_status(name, status, target)
}

/// Format the one report line per harness `canopy skill install` prints.
fn write_status(name: &str, status: InstallStatus, target: &Path) -> Option<String> {
    debug_assert!(target.is_file() && !target.is_symlink());
    let line = match status {
        InstallStatus::Installed => format!("{name}: installed"),
        InstallStatus::UpdatedFrom(old) => format!("{name}: updated from {old}"),
        InstallStatus::Current => format!("{name}: current"),
        InstallStatus::SkippedUnmanaged => {
            format!("{name}: skipped: not managed by canopy")
        }
    };
    Some(line)
}

/// Read the `canopy_version` frontmatter marker, if present.
fn skill_version(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let rest = line.strip_prefix("canopy_version:")?;
        let value = rest.trim().trim_matches('"').trim().to_string();
        (!value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn platform(name: &str, skills_dir: &str) -> Platform {
        serde_json::from_value(serde_json::json!({
            "name": name,
            "config_path": ".config/test",
            "mcp_servers_key": [],
            "skills_dir": skills_dir,
        }))
        .unwrap()
    }

    fn fixture_home() -> (TempDir, Vec<Platform>) {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("alpha/.skills")).unwrap();
        std::fs::create_dir_all(home.path().join("beta/.skills")).unwrap();
        let platforms = vec![
            platform("alpha", "alpha/.skills"),
            platform("beta", "beta/.skills"),
        ];
        (home, platforms)
    }

    fn skill_path(home: &Path, harness: &str) -> std::path::PathBuf {
        home.join(harness)
            .join(".skills")
            .join("canopy")
            .join("SKILL.md")
    }

    #[test]
    fn install_writes_regular_file_with_current_version() {
        let (home, platforms) = fixture_home();
        let lines = install_for_home(home.path(), &platforms, false);

        assert_eq!(lines, vec!["alpha: installed", "beta: installed"]);
        for harness in ["alpha", "beta"] {
            let path = skill_path(home.path(), harness);
            assert!(path.is_file(), "must be a regular file");
            assert!(!path.is_symlink(), "never a symlink");
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.contains(binary_version()));
            assert!(content.contains("name: canopy"));
        }
    }

    #[test]
    fn second_run_reports_current() {
        let (home, platforms) = fixture_home();
        install_for_home(home.path(), &platforms, false);
        let lines = install_for_home(home.path(), &platforms, false);

        assert_eq!(lines, vec!["alpha: current", "beta: current"]);
    }

    #[test]
    fn older_version_is_rewritten_and_reported() {
        let (home, platforms) = fixture_home();
        let path = skill_path(home.path(), "alpha");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "---\nname: canopy\ncanopy_version: 0.0.0\n---\nold\n",
        )
        .unwrap();

        let lines = install_for_home(home.path(), &platforms, false);

        assert_eq!(lines, vec!["alpha: updated from 0.0.0", "beta: installed"]);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(binary_version()));
        assert!(!content.contains("0.0.0"));
    }

    #[test]
    fn hand_written_file_without_version_is_skipped() {
        let (home, platforms) = fixture_home();
        let path = skill_path(home.path(), "alpha");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "---\nname: canopy\n---\nhand written\n").unwrap();

        let lines = install_for_home(home.path(), &platforms, false);

        assert_eq!(
            lines,
            vec!["alpha: skipped: not managed by canopy", "beta: installed"]
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "---\nname: canopy\n---\nhand written\n"
        );
    }

    #[test]
    fn missing_harness_directory_is_not_created() {
        let home = TempDir::new().unwrap();
        let platforms = vec![platform("ghost", "ghost/.skills")];
        let lines = install_for_home(home.path(), &platforms, false);

        assert!(lines.is_empty());
        assert!(!home.path().join("ghost").exists());
    }

    #[test]
    fn only_existing_leaves_new_harnesses_alone() {
        let (home, platforms) = fixture_home();
        let lines = install_for_home(home.path(), &platforms, true);

        assert!(lines.is_empty());
        assert!(!skill_path(home.path(), "alpha").exists());
    }

    #[test]
    fn rendered_frontmatter_names_canopy_and_any_task() {
        let text = rendered();
        assert!(text.starts_with("---\nname: canopy\n"));
        assert!(text.contains("canopy_version: "));
        assert!(!text.contains("{{VERSION}}"));
        let frontmatter = text.split("---").nth(1).unwrap_or_default();
        assert!(frontmatter.contains("ANY task"));
        assert!(frontmatter.contains("canopy skill list"));
        assert!(frontmatter.contains("canopy skill get"));
    }

    #[test]
    fn rendered_body_has_three_sections_and_key_commands() {
        let text = rendered();
        assert!(text.contains(binary_version()));
        let body = text.splitn(3, "---").nth(2).unwrap_or_default();
        assert!(body.contains("## 1."));
        assert!(body.contains("## 2."));
        assert!(body.contains("## 3."));
        for token in [
            "canopy skill list",
            "canopy skill get",
            "get_tools",
            "execution-mindset",
            "architect-mindset",
            "code-engineering",
        ] {
            assert!(body.contains(token), "body must contain {token}");
        }
    }

    #[test]
    fn rendered_body_names_no_skill_besides_the_three_base_ones() {
        let text = rendered();
        let body = text.splitn(3, "---").nth(2).unwrap_or_default();
        assert!(!body.contains("canopy-capabilities"));
        let mut dashed: Vec<&str> = Vec::new();
        let mut rest = body;
        while let Some(start) = rest.find('`') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('`') else {
                break;
            };
            let token = &after[..end];
            if token.contains('-') {
                dashed.push(token);
            }
            rest = &after[end + 1..];
        }
        dashed.sort_unstable();
        dashed.dedup();
        assert_eq!(
            dashed,
            vec!["architect-mindset", "code-engineering", "execution-mindset"]
        );
    }
}
