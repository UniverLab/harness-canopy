//! Instruction-file resolution and the idempotent canopy-protocol amend.
//!
//! The platform registry (UniverLab/canopy-registry) is the source of truth:
//! each platform declares `instruction_file` and `instruction_precedence`.
//! The local baseline sidecar (`~/.canopy/registry-baseline.toml`, written on
//! every registry refresh) is the registry's cached view; when it is missing
//! or does not know the platform, the offline baseline below — the registry's
//! current published values — is the fallback.

use std::path::Path;

use anyhow::{Context, Result};

use crate::setup_module::models::Platform;

/// Delimiters of the canopy protocol block inside an instruction file.
pub const PROTOCOL_BEGIN: &str = "<!-- canopy:protocol:begin -->";
pub const PROTOCOL_END: &str = "<!-- canopy:protocol:end -->";

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
    let block = format!("{PROTOCOL_BEGIN}\n{protocol}\n{PROTOCOL_END}");
    match find_block(content) {
        Some((start, end)) => {
            let mut out = String::with_capacity(content.len() + block.len());
            out.push_str(&content[..start]);
            out.push_str(&block);
            out.push_str(&content[end..]);
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

/// Byte span of the existing protocol block, if present. Both markers must sit
/// at the start of their line. The span is exactly [start of BEGIN, end of END)
/// — the trailing newline after END is deliberately NOT consumed, so replacing
/// the block preserves it and the amend stays idempotent.
fn find_block(content: &str) -> Option<(usize, usize)> {
    let begin = content.find(PROTOCOL_BEGIN)?;
    if begin > 0 && !content[..begin].ends_with('\n') {
        return None;
    }
    let after_begin = begin + PROTOCOL_BEGIN.len();
    let end = after_begin + content[after_begin..].find(PROTOCOL_END)?;
    if end > after_begin && !content[after_begin..end].ends_with('\n') {
        return None;
    }
    Some((begin, end + PROTOCOL_END.len()))
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
            provider: None,
            tool_name: None,
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
}
