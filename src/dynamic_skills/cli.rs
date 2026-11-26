//! `canopy skill` CLI: print the embedded skill, list skills, print one
//! skill with its references, and install the embedded skill.
//!
//! Formatting and suggestion logic lives here; `src/main.rs` only wires the
//! subcommand. `list`/`get` print to caller-supplied writers so tests run
//! in-process without spawning a `canopy` binary. `install` writes the
//! embedded skill as a regular file into every harness skills directory
//! that exists (via [`crate::skills_module::install_for_home`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::dynamic_skills::{CliSkillError, SkillStore};

/// `canopy skill` subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum SkillAction {
    /// List skills from the configured sources.
    List {
        /// Print the report as JSON instead of human-readable text.
        #[arg(long)]
        json: bool,
    },
    /// Print a skill's instructions and reference files to stdout.
    Get {
        /// Name of the skill to print.
        name: String,
    },
    /// Write the embedded `canopy` skill into every harness skills
    /// directory that exists (idempotent: only missing or outdated
    /// copies are rewritten).
    Install,
}

/// Print every known skill, one `<name> - <description>` per line, or a JSON
/// array of `{name, description, source_url}` objects with `--json`.
pub fn run_list(store: &SkillStore, json: bool, out: &mut dyn Write) {
    let entries = store.list();
    if json {
        let items: Vec<serde_json::Value> = entries
            .iter()
            .map(|entry| {
                serde_json::json!({
                    "name": entry.name,
                    "description": entry.description,
                    "source_url": entry.source_url,
                })
            })
            .collect();
        let text = serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
        let _ = writeln!(out, "{text}");
    } else {
        for entry in &entries {
            let _ = writeln!(out, "{} - {}", entry.name, entry.description);
        }
    }
}

/// Run one `canopy skill` invocation: build the store from the on-disk
/// config and dispatch. Exits the process for `get` only — the exit code
/// is the answer. Bare `skill` prints the embedded skill to stdout.
pub fn handle_skill_action(action: Option<SkillAction>) -> anyhow::Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let config = crate::domain::canopy_config::CanopyConfig::load(&data_dir);
    let store = SkillStore::from_config(&data_dir, &config.skills);
    handle_skill_action_with(action, &store, &mut std::io::stdout().lock())
}

/// Dispatch one `canopy skill` invocation with an explicit writer, so the
/// non-exiting branches are testable in-process without spawning a
/// `canopy` binary.
fn handle_skill_action_with(
    action: Option<SkillAction>,
    store: &SkillStore,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match action {
        None => {
            let _ = write!(out, "{}", crate::skills_module::rendered());
            Ok(())
        }
        Some(SkillAction::List { json }) => {
            run_list(store, json, out);
            Ok(())
        }
        Some(SkillAction::Get { name }) => {
            let code = run_get(store, &name, out, &mut std::io::stderr().lock());
            std::process::exit(code);
        }
        Some(SkillAction::Install) => {
            for line in run_install() {
                let _ = writeln!(out, "{line}");
            }
            Ok(())
        }
    }
}

/// Install the embedded `canopy` skill into every harness skills directory
/// that exists on this machine, from the last refreshed registry baseline.
/// Harnesses whose directory does not exist are skipped, never created.
pub fn run_install() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    run_install_in(&home)
}

/// Install into the harnesses recorded in `home/.canopy`'s registry
/// baseline. Split out so tests run against a temp home instead of the
/// real one.
fn run_install_in(home: &Path) -> Vec<String> {
    let baseline = crate::domain::registry_baseline::RegistryBaseline::load(&home.join(".canopy"))
        .unwrap_or_default();
    crate::skills_module::install_for_home(home, &baseline.platforms, false)
}

/// Print a skill to `out` (diagnostics to `err`). Returns the process exit
/// code: 0 on success, 1 when the skill is unknown or unreachable.
pub fn run_get(store: &SkillStore, name: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match store.fetch_for_cli(name) {
        Ok(fetch) => print_cached_skill(&fetch.skill_dir, fetch.served_offline, out, err),
        Err(CliSkillError::NotFound) => {
            print_not_found(store, name, err);
            1
        }
        Err(CliSkillError::Unreachable(cause)) => {
            let _ = writeln!(err, "{cause}");
            1
        }
    }
}

/// Print a fetched skill directory: `SKILL.md` first, then every regular
/// file under `references/` in path order. Files outside `references/` (such
/// as `SKILL.md` itself or the store metadata) are never printed.
fn print_cached_skill(
    skill_dir: &Path,
    served_offline: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    if served_offline {
        let _ = writeln!(err, "(offline: served from cache)");
    }
    let Some(instructions) = crate::skills_module::find_skill_instructions(skill_dir) else {
        let _ = writeln!(err, "skill has no SKILL.md");
        return 1;
    };
    let Ok(bytes) = std::fs::read(&instructions) else {
        let _ = writeln!(err, "skill has no SKILL.md");
        return 1;
    };
    write_text(out, &bytes);
    for (rel, path) in reference_files(skill_dir) {
        let _ = writeln!(out, "=== references/{rel} ===");
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match String::from_utf8(bytes) {
            Ok(text) => write_text(out, text.as_bytes()),
            Err(_) => {
                let _ = writeln!(out, "(binary file skipped: references/{rel})");
            }
        }
    }
    0
}

/// Write bytes to stdout, ensuring exactly one trailing newline.
fn write_text(out: &mut dyn Write, bytes: &[u8]) {
    let _ = out.write_all(bytes);
    if !bytes.ends_with(b"\n") {
        let _ = writeln!(out);
    }
}

/// Regular files under `<skill_dir>/references/`, keyed by their
/// forward-slash path relative to `references/`, sorted lexicographically.
/// Reads use [`std::fs::read`] directly with no size limit.
fn reference_files(skill_dir: &Path) -> Vec<(String, PathBuf)> {
    let refs_dir = skill_dir.join("references");
    let mut files = Vec::new();
    collect_reference_files(&refs_dir, &refs_dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Recurse into real directories only (symlinks are never followed) and
/// collect regular files.
fn collect_reference_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let is_file = entry.file_type().is_ok_and(|t| t.is_file());
        if is_dir {
            collect_reference_files(base, &path, out);
        } else if is_file {
            let rel = path
                .strip_prefix(base)
                .map(forward_slash)
                .unwrap_or_default();
            if !rel.is_empty() {
                out.push((rel, path));
            }
        }
    }
}

/// Render a relative path with forward slashes, whatever the platform.
fn forward_slash(rel: &Path) -> String {
    rel.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Report an unknown skill name, with up to 5 close matches when any exist.
fn print_not_found(store: &SkillStore, name: &str, err: &mut dyn Write) {
    let candidates: Vec<String> = store.list().into_iter().map(|entry| entry.name).collect();
    let suggestions = suggest(name, &candidates);
    if suggestions.is_empty() {
        let _ = writeln!(err, "skill '{name}' not found");
    } else {
        let _ = writeln!(
            err,
            "skill '{name}' not found. Did you mean: {}?",
            suggestions.join(", ")
        );
    }
}

/// Up to 5 candidate names closest to `name` (case-insensitive Levenshtein),
/// ranked by `(distance, candidate)`. A candidate qualifies when its distance
/// is at most half the longer string's length (minimum 2), or when one
/// lowercase string contains the other.
fn suggest(name: &str, candidates: &[String]) -> Vec<String> {
    let lower = name.to_lowercase();
    let mut scored: Vec<(usize, &str)> = Vec::new();
    for candidate in candidates {
        let cand_lower = candidate.to_lowercase();
        let distance = lev(&lower, &cand_lower);
        let max_len = lower.chars().count().max(cand_lower.chars().count());
        if distance <= (max_len / 2).max(2)
            || lower.contains(cand_lower.as_str())
            || cand_lower.contains(lower.as_str())
        {
            scored.push((distance, candidate.as_str()));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(5)
        .map(|(_, candidate)| candidate.to_string())
        .collect()
}

/// Levenshtein edit distance between two strings (two-row dynamic program).
fn lev(first: &str, second: &str) -> usize {
    let a: Vec<char> = first.chars().collect();
    let b: Vec<char> = second.chars().collect();
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len());
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, &char_a) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &char_b) in b.iter().enumerate() {
            let cost = usize::from(char_a != char_b);
            curr[j + 1] = (prev[j] + cost).min(curr[j] + 1).min(prev[j + 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_skills::GitSource;
    use std::process::Command;
    use tempfile::TempDir;

    fn test_source(dir: &Path) -> GitSource {
        GitSource::new(dir.to_string_lossy().to_string(), None)
    }

    fn init_repo(dir: &Path) {
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

    fn skill_markdown(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: \"{description}\"\n---\n# {name}\n")
    }

    /// Create a skill directory holding `SKILL.md` plus extra files given as
    /// `(relative path, bytes)` pairs (parents created as needed).
    fn write_skill(dir: &Path, name: &str, description: &str, files: &[(&str, &[u8])]) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            skill_markdown(name, description),
        )
        .unwrap();
        for (rel, bytes) in files {
            let path = skill_dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
    }

    fn registry_with(skills: &[(&str, &str)]) -> TempDir {
        let registry = TempDir::new().unwrap();
        init_repo(registry.path());
        for (name, description) in skills {
            write_skill(registry.path(), name, description, &[]);
        }
        commit_all(registry.path(), "init");
        registry
    }

    #[test]
    fn list_prints_name_dash_description_lines() {
        let registry = registry_with(&[("alpha", "Alpha skill."), ("beta", "Beta skill.")]);
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        run_list(&store, false, &mut out);

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "alpha - Alpha skill.\nbeta - Beta skill.\n"
        );
    }

    #[test]
    fn list_json_prints_name_description_source_url_objects() {
        let registry = registry_with(&[("alpha", "Alpha skill."), ("beta", "Beta skill.")]);
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        run_list(&store, true, &mut out);

        let items: Vec<serde_json::Value> =
            serde_json::from_slice(&out).expect("list --json must print a JSON array");
        assert_eq!(items.len(), 2);
        let source_url = registry.path().to_string_lossy().to_string();
        for item in &items {
            let mut keys: Vec<&str> = item
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, vec!["description", "name", "source_url"]);
            assert_eq!(item["source_url"], source_url.as_str());
        }
        assert_eq!(items[0]["name"], "alpha");
        assert_eq!(items[0]["description"], "Alpha skill.");
        assert_eq!(items[1]["name"], "beta");
    }

    #[test]
    fn get_prints_skill_md_then_references_in_path_order() {
        let registry = TempDir::new().unwrap();
        init_repo(registry.path());
        write_skill(
            registry.path(),
            "alpha",
            "Alpha skill.",
            &[
                ("references/b.md", b"B\n"),
                ("references/a.md", b"A"),
                ("references/sub/c.md", b"C\n"),
                ("notes.txt", b"NOT PRINTED\n"),
            ],
        );
        commit_all(registry.path(), "init");
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_get(&store, "alpha", &mut out, &mut err);

        assert_eq!(code, 0);
        assert!(err.is_empty());
        let stdout = String::from_utf8(out).unwrap();
        assert_eq!(
            stdout,
            concat!(
                "---\nname: alpha\ndescription: \"Alpha skill.\"\n---\n# alpha\n",
                "=== references/a.md ===\nA\n",
                "=== references/b.md ===\nB\n",
                "=== references/sub/c.md ===\nC\n",
            )
        );
    }

    #[test]
    fn get_prints_large_references_fully_and_skips_binary() {
        let registry = TempDir::new().unwrap();
        init_repo(registry.path());
        let big = vec![b'x'; 70_000];
        write_skill(
            registry.path(),
            "alpha",
            "Alpha skill.",
            &[
                ("references/big.md", &big),
                ("references/blob.bin", b"\xff\xfe\x00binary"),
                ("extra.txt", b"EXTRA\n"),
            ],
        );
        commit_all(registry.path(), "init");
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_get(&store, "alpha", &mut out, &mut err);

        assert_eq!(code, 0);
        let stdout = String::from_utf8_lossy(&out).to_string();
        let mut expected = skill_markdown("alpha", "Alpha skill.");
        expected.push_str("=== references/big.md ===\n");
        expected.push_str(&"x".repeat(70_000));
        expected.push('\n');
        expected
            .push_str("=== references/blob.bin ===\n(binary file skipped: references/blob.bin)\n");
        assert_eq!(stdout, expected);
        assert!(!stdout.contains("EXTRA"));
    }

    #[test]
    fn get_unknown_name_suggests_close_matches() {
        let registry = registry_with(&[
            ("deploy-app", "Deploy app."),
            ("deploy-web", "Deploy web."),
            ("deploy-api", "Deploy api."),
            ("test-runner", "Run tests."),
            ("docs-writer", "Write docs."),
            ("lint-check", "Lint."),
            ("format-code", "Format."),
        ]);
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_get(&store, "deply", &mut out, &mut err);

        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "skill 'deply' not found. Did you mean: deploy-api, deploy-app, deploy-web?\n"
        );
    }

    #[test]
    fn get_unknown_name_without_matches_reports_plain_not_found() {
        let registry = registry_with(&[("alpha", "Alpha skill.")]);
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_get(&store, "zzzzqqq", &mut out, &mut err);

        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "skill 'zzzzqqq' not found\n"
        );
    }

    #[test]
    fn get_offline_serves_cache_and_marks_stderr() {
        let root = TempDir::new().unwrap();
        let registry_path = root.path().join("registry");
        std::fs::create_dir(&registry_path).unwrap();
        init_repo(&registry_path);
        write_skill(&registry_path, "alpha", "Alpha skill.", &[]);
        commit_all(&registry_path, "init");
        // TTL 0 forces a source refresh check on every call.
        let store = SkillStore::new(
            root.path().join("store"),
            vec![test_source(&registry_path)],
            0,
        );

        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run_get(&store, "alpha", &mut out, &mut err), 0);
        assert!(err.is_empty());

        let moved = root.path().join("registry-moved");
        std::fs::rename(&registry_path, &moved).unwrap();

        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run_get(&store, "alpha", &mut out, &mut err), 0);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "(offline: served from cache)\n"
        );
        assert!(String::from_utf8(out).unwrap().contains("# alpha"));

        assert!(!root.path().join(".agents").exists());
        assert!(!root.path().join(".canopy").join("skills").exists());

        std::fs::rename(&moved, &registry_path).unwrap();
    }

    #[test]
    fn get_unreachable_without_cache_fails_with_cause() {
        let root = TempDir::new().unwrap();
        let missing = root.path().join("no-such-registry");
        let store = SkillStore::new(root.path().join("store"), vec![test_source(&missing)], 15);

        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_get(&store, "alpha", &mut out, &mut err);

        assert_eq!(code, 1);
        assert!(out.is_empty());
        let stderr = String::from_utf8(err).unwrap();
        assert!(
            stderr.contains("no-such-registry"),
            "cause must name the source, got: {stderr}"
        );

        assert!(!root.path().join(".agents").exists());
        assert!(!root.path().join(".canopy").join("skills").exists());
    }

    #[test]
    fn lev_matches_known_distances() {
        assert_eq!(lev("kitten", "sitting"), 3);
        assert_eq!(lev("", "abc"), 3);
        assert_eq!(lev("abc", ""), 3);
        assert_eq!(lev("same", "same"), 0);
        assert_eq!(lev("abc", "abd"), 1);
    }

    #[test]
    fn bare_skill_prints_the_embedded_canopy_skill() {
        let registry = registry_with(&[("alpha", "Alpha skill.")]);
        let store_dir = TempDir::new().unwrap();
        let store = SkillStore::new(
            store_dir.path().to_path_buf(),
            vec![test_source(registry.path())],
            15,
        );

        let mut out = Vec::new();
        handle_skill_action_with(None, &store, &mut out).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, crate::skills_module::rendered());
        assert!(text.starts_with("---\nname: canopy\n"));
        assert!(text.contains("canopy skill list"));
    }

    #[test]
    fn install_writes_baseline_harnesses_and_reports_status() {
        use crate::domain::registry_baseline::RegistryBaseline;

        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("alpha/.skills")).unwrap();
        std::fs::create_dir_all(home.path().join("beta/.skills")).unwrap();
        let platform = |name: &str, skills_dir: &str| {
            serde_json::from_value::<crate::setup_module::Platform>(serde_json::json!({
                "name": name,
                "config_path": ".config/test",
                "mcp_servers_key": [],
                "skills_dir": skills_dir,
            }))
            .unwrap()
        };
        let baseline = RegistryBaseline {
            clis: Vec::new(),
            platforms: vec![
                platform("alpha", "alpha/.skills"),
                platform("beta", "beta/.skills"),
            ],
        };
        baseline.save(&home.path().join(".canopy")).unwrap();

        assert_eq!(
            run_install_in(home.path()),
            vec!["alpha: installed", "beta: installed"]
        );
        for harness in ["alpha", "beta"] {
            let path = home
                .path()
                .join(harness)
                .join(".skills")
                .join("canopy")
                .join("SKILL.md");
            assert!(path.is_file() && !path.is_symlink());
        }
        assert_eq!(
            run_install_in(home.path()),
            vec!["alpha: current", "beta: current"]
        );
    }

    #[test]
    fn suggest_is_case_insensitive_ranks_and_caps_at_five() {
        let candidates: Vec<String> = [
            "alpha", "alpha2", "alpha3", "alpha4", "alpha5", "alpha6", "alphb", "zzz",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            suggest("ALPHA", &candidates),
            vec!["alpha", "alpha2", "alpha3", "alpha4", "alpha5"]
        );
        assert_eq!(
            suggest("app", &["my-app-skill".to_string(), "zzz".to_string()]),
            vec!["my-app-skill".to_string()]
        );
        let empty: Vec<String> = Vec::new();
        assert!(suggest("alpha", &empty).is_empty());
    }
}
