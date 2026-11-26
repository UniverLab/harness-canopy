//! Instruction-file resolution and the idempotent canopy-protocol amend.
//!
//! The platform registry (UniverLab/canopy-registry) is the source of truth:
//! each platform declares `instruction_file` and `instruction_precedence`.
//! The local baseline sidecar (`~/.canopy/registry-baseline.toml`, written on
//! every registry refresh) is the registry's cached view; when it is missing
//! or does not know the platform, the offline baseline below — the registry's
//! current published values — is the fallback.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

use crate::setup_module::models::Platform;

/// Delimiters of the canopy protocol block inside an instruction file.
pub const PROTOCOL_BEGIN: &str = "<!-- canopy:protocol:begin -->";
pub const PROTOCOL_END: &str = "<!-- canopy:protocol:end -->";

/// Delimiters and line for the canopy skill pointer inside a user-level instruction file.
pub const SKILL_BEGIN: &str = "<!-- canopy:skill:begin -->";
pub const SKILL_END: &str = "<!-- canopy:skill:end -->";
pub const SKILL_LINE: &str =
    "At the start of every task, use the canopy skill (run: canopy skill).";

static SKILL_LINE_LOCK: Mutex<()> = Mutex::new(());

/// Where a harness reads its instructions: the file to amend/create when no
/// precedence path exists, plus the paths checked first, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSpec {
    /// File to amend (or create) when no precedence path exists in the worktree.
    pub instruction_file: String,
    /// Paths the harness actually reads, in order; the first one that exists
    /// in the worktree is amended. Empty for platforms with no known precedence.
    pub instruction_precedence: Vec<String>,
}

/// Resolve the instruction spec for `cli_name`: the registry's view when the
/// baseline sidecar knows the platform, otherwise the offline baseline.
pub fn instruction_spec_for_cli(cli_name: &str) -> InstructionSpec {
    instruction_spec_for(cli_name, registry_platform(cli_name).as_ref())
}

/// Testable core: resolve against an explicit registry view (`None` = the
/// registry does not know this platform). A known platform with an empty
/// `instruction_file` / `instruction_precedence` falls back per-field to the
/// offline baseline.
pub(crate) fn instruction_spec_for(cli_name: &str, platform: Option<&Platform>) -> InstructionSpec {
    let offline = offline_spec(cli_name);
    let Some(platform) = platform else {
        return offline;
    };
    InstructionSpec {
        instruction_file: platform
            .instruction_file
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or(offline.instruction_file),
        instruction_precedence: if platform.instruction_precedence.is_empty() {
            offline.instruction_precedence
        } else {
            platform.instruction_precedence.clone()
        },
    }
}

/// The registry's cached view of one platform, from the baseline sidecar.
/// `None` when the sidecar is absent, unparseable, or does not list the
/// platform — all mean "no registry answer", which the offline baseline covers.
fn registry_platform(name: &str) -> Option<Platform> {
    let baseline = crate::domain::registry_baseline::RegistryBaseline::load(
        &crate::domain::prompts::canopy_dir(),
    )?;
    baseline.get_platform(name).cloned()
}

/// Offline baseline: the registry's current published values, used when the
/// baseline sidecar is missing or does not know the platform. These are the
/// REGISTRY's values (claude → AGENTS.md), not the removed hard-coded table's
/// values — the table's claude → CLAUDE.md was the drift this spec removes.
fn offline_spec(cli_name: &str) -> InstructionSpec {
    let (instruction_file, precedence): (&str, &[&str]) = match cli_name {
        "claude" => (
            "AGENTS.md",
            &["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"],
        ),
        "codex" => ("AGENTS.md", &["AGENTS.override.md"]),
        "gemini" => ("GEMINI.md", &[]),
        "copilot" => (".github/copilot-instructions.md", &[]),
        // opencode, kiro, mistral, and anything unknown.
        _ => ("AGENTS.md", &[]),
    };
    InstructionSpec {
        instruction_file: instruction_file.to_string(),
        instruction_precedence: precedence.iter().map(|s| s.to_string()).collect(),
    }
}

/// Idempotently amend `content` with the canopy protocol: the protocol lives
/// in one delimited block. An existing block is replaced in place (the rest of
/// the file is byte-identical, including any newline after the end marker);
/// otherwise the block is appended at the end after one blank line. Running it
/// twice with the same protocol returns the same string as running it once.
pub fn amend_with_protocol(content: &str, protocol: &str) -> String {
    amend_block(content, protocol, PROTOCOL_BEGIN, PROTOCOL_END)
}

/// Idempotently amend `content` with the canopy skill pointer line in its own
/// delimited block (`SKILL_BEGIN` / `SKILL_END`), using the same replacement
/// and append rules as [`amend_with_protocol`].
pub fn amend_with_skill_line(content: &str, line: &str) -> String {
    amend_block(content, line, SKILL_BEGIN, SKILL_END)
}

/// Remove the `SKILL_BEGIN`..`SKILL_END` block and the blank line added before
/// it, returning `None` if no skill block is present.
pub fn remove_skill_block(content: &str) -> Option<String> {
    let (start, end) = find_block(content, SKILL_BEGIN, SKILL_END)?;
    let before = &content[..start];
    let after = &content[end..];
    if after.trim_matches('\n').is_empty() {
        if before.trim_matches('\n').is_empty() {
            return Some(String::new());
        }
        return Some(format!("{}\n", before.trim_end_matches('\n')));
    }
    let after_trimmed = after.strip_prefix('\n').unwrap_or(after);
    if before.is_empty() {
        return Some(after_trimmed.trim_start_matches('\n').to_string());
    }
    if before.ends_with("\n\n") && after_trimmed.starts_with('\n') {
        return Some(format!("{}{}", &before[..before.len() - 1], after_trimmed));
    }
    Some(format!("{before}{after_trimmed}"))
}

fn amend_block(content: &str, body: &str, begin: &str, end: &str) -> String {
    let block = format!("{begin}\n{body}\n{end}");
    match find_block(content, begin, end) {
        Some((start, span_end)) => {
            let mut out = String::with_capacity(content.len() + block.len());
            out.push_str(&content[..start]);
            out.push_str(&block);
            out.push_str(&content[span_end..]);
            out
        }
        None => {
            let trimmed = content.trim_end_matches('\n');
            if trimmed.is_empty() {
                block
            } else {
                format!("{trimmed}\n\n{block}\n")
            }
        }
    }
}

/// Byte span of an existing delimited block, if present. Both markers must sit
/// at the start of their line. The span is exactly [start of BEGIN, end of END)
/// — the trailing newline after END is deliberately NOT consumed, so replacing
/// the block preserves it and the amend stays idempotent.
fn find_block(content: &str, begin_marker: &str, end_marker: &str) -> Option<(usize, usize)> {
    let begin = content.find(begin_marker)?;
    if begin > 0 && !content[..begin].ends_with('\n') {
        return None;
    }
    let after_begin = begin + begin_marker.len();
    let end = after_begin + content[after_begin..].find(end_marker)?;
    if end > after_begin && !content[after_begin..end].ends_with('\n') {
        return None;
    }
    Some((begin, end + end_marker.len()))
}

/// Resolve a relative `user_instruction_file` field against `home`, rejecting
/// empty strings, absolute paths, or any `..` / root / prefix component.
pub(crate) fn resolve_user_instruction_path(home: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return None;
    }
    if rel_path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(home.join(rel_path))
}

/// Resolve the harness's user-level instruction file from the cached registry
/// baseline under `home/.canopy`. Returns `None` when the platform is unknown,
/// has no `user_instruction_file`, or the field is empty / unsafe.
pub fn user_instruction_file(cli_name: &str, home: &Path) -> Option<PathBuf> {
    let baseline = crate::domain::registry_baseline::RegistryBaseline::load(&home.join(".canopy"))?;
    let platform = baseline.get_platform(cli_name)?;
    let rel = platform.user_instruction_file.as_deref()?;
    resolve_user_instruction_path(home, rel)
}

/// Ensure the harness's existing regular user-level instruction file carries
/// `SKILL_LINE`. Never creates files or directories, skips symlinks and
/// non-regular files (logging at DEBUG), and logs errors at WARN with path.
pub fn ensure_skill_line(cli_name: &str, home: &Path) {
    let _guard = SKILL_LINE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let Some(path) = user_instruction_file(cli_name, home) else {
        tracing::debug!(
            cli = cli_name,
            "skipping skill line: no user_instruction_file"
        );
        return;
    };

    if let Some(meta) = stat_regular_file(&path) {
        amend_skill_line_file(&path, meta.permissions());
    }
}

fn stat_regular_file(path: &Path) -> Option<std::fs::Metadata> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "skipping skill line: file does not exist");
            return None;
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to stat user instruction file: {e}");
            return None;
        }
    };
    if let Some(reason) = non_regular_reason(&meta) {
        tracing::debug!(path = %path.display(), "skipping skill line: {reason}");
        return None;
    }
    Some(meta)
}

fn non_regular_reason(meta: &std::fs::Metadata) -> Option<&'static str> {
    if meta.file_type().is_symlink() {
        Some("target is a symlink")
    } else if !meta.file_type().is_file() {
        Some("target is not a regular file")
    } else {
        None
    }
}

fn amend_skill_line_file(path: &Path, permissions: std::fs::Permissions) {
    let existing = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), "failed to read user instruction file: {e}");
            return;
        }
    };
    let amended = amend_with_skill_line(&existing, SKILL_LINE);
    if amended == existing {
        return;
    }
    if let Err(e) = write_preserving_permissions(path, &amended, permissions) {
        tracing::warn!(path = %path.display(), "failed to update user instruction file: {e}");
    }
}

fn write_preserving_permissions(
    path: &Path,
    content: &str,
    permissions: std::fs::Permissions,
) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    tmp.as_file().set_permissions(permissions)?;
    tmp.persist(path).map_err(|e| e.error)?; // lint-pattern
    Ok(())
}

/// The instruction file to amend inside `worktree`: the first existing path in
/// the spec's precedence list, else its instruction file (created if missing).
pub fn resolve_target(worktree: &Path, spec: &InstructionSpec) -> std::path::PathBuf {
    for rel in &spec.instruction_precedence {
        let candidate = worktree.join(rel);
        if candidate.exists() {
            return candidate;
        }
    }
    worktree.join(&spec.instruction_file)
}

/// Amend the instruction file inside `worktree` with `protocol` and keep it
/// out of git: a tracked target is marked `skip-worktree` (info/exclude does
/// not apply to tracked files), an untracked one goes into the worktree's
/// info/exclude. The skip-worktree mark is set AFTER the amend.
pub async fn write_protocol_file(
    worktree: &Path,
    spec: &InstructionSpec,
    protocol: &str,
) -> Result<()> {
    let target = resolve_target(worktree, spec);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create instruction parent dir: {}",
                parent.display()
            )
        })?;
    }

    // Read-amend-write: amend() replaces or appends, it never rewrites the
    // project's own instructions (defect 1).
    let existing = std::fs::read_to_string(&target).unwrap_or_default();
    let amended = amend_with_protocol(&existing, protocol);
    std::fs::write(&target, amended)
        .with_context(|| format!("Failed to write instruction file: {}", target.display()))?;

    let rel = target.strip_prefix(worktree).unwrap_or(&target);
    if is_tracked(worktree, &target).await.unwrap_or_else(|e| {
        tracing::warn!(
            "could not determine whether {} is tracked: {e:#}",
            target.display()
        );
        false
    }) {
        mark_skip_worktree(worktree, &target).await;
    } else {
        exclude_from_worktree(worktree, &rel.to_string_lossy()).await;
    }
    Ok(())
}

/// Whether git tracks `path` (`git ls-files --error-unmatch`). Run inside the
/// worktree with the path relative to it.
async fn is_tracked(worktree: &Path, path: &Path) -> Result<bool> {
    let rel = path.strip_prefix(worktree).unwrap_or(path);
    let output = tokio::process::Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", &rel.to_string_lossy()])
        .current_dir(worktree)
        .output()
        .await
        .context("Failed to run git ls-files")?;
    Ok(output.status.success())
}

/// Best-effort `git update-index --skip-worktree`: a failure here just loses
/// that one safeguard (same philosophy as `exclude_from_worktree`).
async fn mark_skip_worktree(worktree: &Path, path: &Path) {
    let rel = path.strip_prefix(worktree).unwrap_or(path);
    match tokio::process::Command::new("git")
        .args([
            "update-index",
            "--skip-worktree",
            "--",
            &rel.to_string_lossy(),
        ])
        .current_dir(worktree)
        .output()
        .await
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(
            "git update-index --skip-worktree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(e) => tracing::warn!("failed to run git update-index: {e:#}"),
    }
}

/// Append `pattern` to the worktree's git exclude file so a broad `git add`
/// in a graph/session node cannot pull the (uncommitted) instruction file into
/// a commit. Best-effort: a failure here just loses that one safeguard.
/// (Moved verbatim from `sandbox.rs`.)
async fn exclude_from_worktree(worktree_path: &std::path::Path, pattern: &str) {
    let Ok(output) = tokio::process::Command::new("git")
        .args(["rev-parse", "--git-path", "info/exclude"])
        .current_dir(worktree_path)
        .output()
        .await
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let rel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rel.is_empty() {
        return;
    }
    let exclude_path = worktree_path.join(rel);
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    if !existing.lines().any(|l| l.trim() == pattern) {
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str(pattern);
        existing.push('\n');
        let _ = std::fs::write(&exclude_path, existing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform(instruction_file: Option<&str>, precedence: Vec<&str>) -> Platform {
        Platform {
            name: "claude".to_string(),
            config_path: ".claude/settings.json".to_string(),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: std::collections::HashMap::new(),
            required_fields: std::collections::HashMap::new(),
            server_extras: std::collections::HashMap::new(),
            skills_dir: None,
            instruction_file: instruction_file.map(str::to_string),
            instruction_precedence: precedence.into_iter().map(str::to_string).collect(),
            user_instruction_file: None,
            provider: None,
            tool_name: None,
            project_paths: Vec::new(),
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: Vec::new(),
            install_commands: Vec::new(),
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
            cli: None,
        }
    }

    #[test]
    fn amend_appends_block_to_empty_content() {
        assert_eq!(
            amend_with_protocol("", "P"),
            "<!-- canopy:protocol:begin -->\nP\n<!-- canopy:protocol:end -->"
        );
    }

    #[test]
    fn amend_appends_after_one_blank_line() {
        let expected =
            "# Rules\n\n<!-- canopy:protocol:begin -->\nP\n<!-- canopy:protocol:end -->\n";
        assert_eq!(amend_with_protocol("# Rules\n", "P"), expected);
        assert_eq!(amend_with_protocol("# Rules", "P"), expected);
    }

    #[test]
    fn amend_replaces_block_in_place() {
        let content =
            "# Rules\n\n<!-- canopy:protocol:begin -->\nOLD\n<!-- canopy:protocol:end -->\n\ntail\n";
        assert_eq!(
            amend_with_protocol(content, "NEW"),
            "# Rules\n\n<!-- canopy:protocol:begin -->\nNEW\n<!-- canopy:protocol:end -->\n\ntail\n"
        );
    }

    #[test]
    fn amend_is_idempotent() {
        let without_block = "# Rules\n";
        let once = amend_with_protocol(without_block, "P");
        assert_eq!(amend_with_protocol(&once, "P"), once);

        let with_block = amend_with_protocol("# Rules\n", "OLD");
        let twice = amend_with_protocol(&with_block, "OLD");
        assert_eq!(twice, with_block);
    }

    #[test]
    fn amend_replaces_stale_block_without_duplicating() {
        let content = amend_with_protocol("# Rules\n", "OLD_PROTOCOL");
        let result = amend_with_protocol(&content, "NEW_PROTOCOL");
        assert_eq!(result.matches(PROTOCOL_BEGIN).count(), 1);
        assert_eq!(result.matches(PROTOCOL_END).count(), 1);
        assert!(result.contains("NEW_PROTOCOL"));
        assert!(!result.contains("OLD_PROTOCOL"));
    }

    #[test]
    fn resolve_target_prefers_first_existing_precedence_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "x").unwrap();
        let spec = InstructionSpec {
            instruction_file: "AGENTS.md".to_string(),
            instruction_precedence: vec![
                "CLAUDE.md".to_string(),
                ".claude/CLAUDE.md".to_string(),
                "CLAUDE.local.md".to_string(),
            ],
        };
        assert_eq!(
            resolve_target(dir.path(), &spec),
            dir.path().join("CLAUDE.md")
        );
    }

    #[test]
    fn resolve_target_takes_second_precedence_path_when_first_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(dir.path().join(".claude/CLAUDE.md"), "x").unwrap();
        let spec = InstructionSpec {
            instruction_file: "AGENTS.md".to_string(),
            instruction_precedence: vec![
                "CLAUDE.md".to_string(),
                ".claude/CLAUDE.md".to_string(),
                "CLAUDE.local.md".to_string(),
            ],
        };
        assert_eq!(
            resolve_target(dir.path(), &spec),
            dir.path().join(".claude/CLAUDE.md")
        );
    }

    #[test]
    fn resolve_target_falls_back_to_instruction_file() {
        let dir = tempfile::tempdir().unwrap();
        let spec = InstructionSpec {
            instruction_file: "AGENTS.md".to_string(),
            instruction_precedence: vec!["CLAUDE.md".to_string()],
        };
        assert_eq!(
            resolve_target(dir.path(), &spec),
            dir.path().join("AGENTS.md")
        );
    }

    #[test]
    fn instruction_spec_prefers_registry_platform() {
        let p = platform(Some("CUSTOM.md"), vec!["X.md"]);
        let spec = instruction_spec_for("claude", Some(&p));
        assert_eq!(spec.instruction_file, "CUSTOM.md");
        assert_eq!(spec.instruction_precedence, vec!["X.md".to_string()]);
    }

    #[test]
    fn instruction_spec_falls_back_per_field() {
        let p = platform(None, vec![]);
        let spec = instruction_spec_for("claude", Some(&p));
        assert_eq!(spec.instruction_file, "AGENTS.md");
        assert_eq!(
            spec.instruction_precedence,
            vec![
                "CLAUDE.md".to_string(),
                ".claude/CLAUDE.md".to_string(),
                "CLAUDE.local.md".to_string()
            ]
        );
    }

    #[test]
    fn instruction_spec_offline_baseline_values() {
        let claude = instruction_spec_for("claude", None);
        assert_eq!(claude.instruction_file, "AGENTS.md");
        assert_eq!(
            claude.instruction_precedence,
            vec![
                "CLAUDE.md".to_string(),
                ".claude/CLAUDE.md".to_string(),
                "CLAUDE.local.md".to_string()
            ]
        );

        let codex = instruction_spec_for("codex", None);
        assert_eq!(codex.instruction_file, "AGENTS.md");
        assert_eq!(
            codex.instruction_precedence,
            vec!["AGENTS.override.md".to_string()]
        );

        assert_eq!(
            instruction_spec_for("gemini", None).instruction_file,
            "GEMINI.md"
        );
        assert_eq!(
            instruction_spec_for("copilot", None).instruction_file,
            ".github/copilot-instructions.md"
        );
        assert_eq!(
            instruction_spec_for("opencode", None).instruction_file,
            "AGENTS.md"
        );
        assert_eq!(
            instruction_spec_for("nonexistent", None).instruction_file,
            "AGENTS.md"
        );
    }

    fn write_baseline_with_user_file(home: &Path, cli: &str, user_instruction_file: Option<&str>) {
        let mut p = platform(None, vec![]);
        p.name = cli.to_string();
        p.user_instruction_file = user_instruction_file.map(str::to_string);
        crate::domain::registry_baseline::RegistryBaseline {
            clis: vec![],
            platforms: vec![p],
        }
        .save(&home.join(".canopy"))
        .unwrap();
    }

    #[test]
    fn skill_line_missing_file_and_dir_not_created() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", Some(".testcli/USER.md"));

        ensure_skill_line("testcli", home.path());

        assert!(!home.path().join(".testcli").exists());
        assert!(!home.path().join(".testcli/USER.md").exists());
    }

    #[test]
    fn skill_line_appends_to_existing_file() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", Some(".testcli/USER.md"));
        let target = home.path().join(".testcli/USER.md");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "# My Rules\n").unwrap();

        ensure_skill_line("testcli", home.path());

        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            content,
            format!("# My Rules\n\n{SKILL_BEGIN}\n{SKILL_LINE}\n{SKILL_END}\n")
        );
    }

    #[test]
    fn skill_line_second_call_is_byte_identical() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", Some(".testcli/USER.md"));
        let target = home.path().join(".testcli/USER.md");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "# My Rules\n").unwrap();

        ensure_skill_line("testcli", home.path());
        let first = std::fs::read(&target).unwrap();
        ensure_skill_line("testcli", home.path());
        let second = std::fs::read(&target).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn skill_line_replaces_changed_line_in_place_preserving_surrounding_text() {
        let initial = format!("# Before\n\n{SKILL_BEGIN}\nOld line.\n{SKILL_END}\n\n# After\n");
        let updated = amend_with_skill_line(&initial, SKILL_LINE);
        assert_eq!(
            updated,
            format!("# Before\n\n{SKILL_BEGIN}\n{SKILL_LINE}\n{SKILL_END}\n\n# After\n")
        );
        assert_eq!(updated.matches(SKILL_BEGIN).count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn skill_line_skips_symlink() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", Some(".testcli/USER.md"));
        std::fs::create_dir_all(home.path().join(".testcli")).unwrap();
        let real = home.path().join("real.md");
        std::fs::write(&real, "# Original\n").unwrap();
        let link = home.path().join(".testcli/USER.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        ensure_skill_line("testcli", home.path());

        assert_eq!(std::fs::read_to_string(&real).unwrap(), "# Original\n");
    }

    #[test]
    fn skill_line_absent_field_leaves_untouched() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", None);
        let file = home.path().join(".testcli/USER.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# Untouched\n").unwrap();

        assert_eq!(user_instruction_file("testcli", home.path()), None);
        ensure_skill_line("testcli", home.path());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Untouched\n");
    }

    #[test]
    fn skill_line_rejects_absolute_and_parent_dir_paths() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "abscli", Some("/etc/passwd"));
        assert_eq!(user_instruction_file("abscli", home.path()), None);

        write_baseline_with_user_file(home.path(), "dotcli", Some("../escape.md"));
        assert_eq!(user_instruction_file("dotcli", home.path()), None);

        write_baseline_with_user_file(home.path(), "midcli", Some(".testcli/../../escape.md"));
        assert_eq!(user_instruction_file("midcli", home.path()), None);

        write_baseline_with_user_file(home.path(), "emptycli", Some(""));
        assert_eq!(user_instruction_file("emptycli", home.path()), None);
    }

    #[test]
    fn skill_line_concurrent_calls_leave_exactly_one_block() {
        let home = tempfile::tempdir().unwrap();
        write_baseline_with_user_file(home.path(), "testcli", Some(".testcli/USER.md"));
        let target = home.path().join(".testcli/USER.md");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "# Concurrent\n").unwrap();

        std::thread::scope(|s| {
            let h1 = s.spawn(|| ensure_skill_line("testcli", home.path()));
            let h2 = s.spawn(|| ensure_skill_line("testcli", home.path()));
            h1.join().unwrap();
            h2.join().unwrap();
        });

        let content = std::fs::read_to_string(&target).unwrap();
        assert_eq!(content.matches(SKILL_BEGIN).count(), 1);
        assert_eq!(content.matches(SKILL_END).count(), 1);
    }
}
