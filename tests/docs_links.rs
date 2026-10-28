//! Documentation link check: every relative link and `#anchor` in `docs/*.md`
//! and `README.md` must resolve against the file it points at.
//!
//! The anchor is computed the way GitHub builds heading slugs: lowercase,
//! drop everything outside `[a-z0-9 _-]`, spaces to `-`, and `-1`, `-2` …
//! for repeated headings. Links inside fenced code blocks are ignored.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn strip_fences(text: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn slug(heading: &str) -> String {
    let kept: String = heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect();
    kept.trim().replace(' ', "-")
}

fn anchors(text: &str) -> HashSet<String> {
    let body = strip_fences(text);
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut out = HashSet::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !rest.starts_with(' ') {
            continue;
        }
        let base = slug(rest.trim());
        let count = seen.entry(base.clone()).or_insert(0);
        out.insert(if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        });
        *count += 1;
    }
    out
}

/// Every `](target)` in the text, as `(target, line number)`.
fn link_targets(text: &str) -> Vec<(String, usize)> {
    let body = strip_fences(text);
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(found) = body[offset..].find("](") {
        let start = offset + found + 2;
        let Some(close) = body[start..].find(')') else {
            break;
        };
        let target = body[start..start + close].trim().to_string();
        let line = body[..start].lines().count();
        out.push((target, line));
        offset = start + close;
    }
    out
}

fn markdown_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files: Vec<PathBuf> = fs::read_dir(root.join("docs"))
        .expect("docs/ must exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files.push(root.join("README.md"));
    files
}

fn anchor_cache(path: &Path) -> HashSet<String> {
    anchors(&fs::read_to_string(path).expect("documentation file must be readable"))
}

#[test]
fn every_relative_link_and_anchor_resolves() {
    let files = markdown_files();
    assert!(files.len() > 10, "expected the docs pages, found {files:?}");
    let mut cache: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    let mut failures = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("documentation file must be readable");
        for (target, line) in link_targets(&text) {
            if target.is_empty()
                || target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let url = target.split_whitespace().next().unwrap_or("");
            let (path, fragment) = match url.split_once('#') {
                Some((path, fragment)) => (path, Some(fragment)),
                None => (url, None),
            };
            let resolved = if path.is_empty() {
                file.clone()
            } else {
                file.parent().expect("file has a parent").join(path)
            };
            if !resolved.exists() {
                failures.push(format!(
                    "{}:{line}: link `{target}` points at a missing file",
                    file.display()
                ));
                continue;
            }
            if let Some(fragment) = fragment {
                let known = cache
                    .entry(resolved.clone())
                    .or_insert_with(|| anchor_cache(&resolved));
                if !known.contains(fragment) {
                    failures.push(format!(
                        "{}:{line}: link `{target}` points at a missing anchor",
                        file.display()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "unresolved documentation links:\n{}",
        failures.join("\n")
    );
}

/// The engine reference is split into one page per question: `graphs.md` is
/// the overview, the sibling pages carry the sections, and the two playbook
/// pages moved to the graphs catalogue.
#[test]
fn graph_reference_is_split_into_focused_pages() {
    let docs = repo_root().join("docs");
    let pages = [
        ("graphs.md", 150usize),
        ("graphs-templates.md", 400),
        ("graphs-ensembles.md", 400),
        ("graphs-execution.md", 400),
        ("graphs-isolation.md", 400),
        ("graphs-hooks-and-queues.md", 400),
        ("graphs-sharing.md", 400),
        ("graphs-nodes.md", 400),
        ("graphs-operations.md", 400),
    ];
    for (name, max_lines) in pages {
        let text = fs::read_to_string(docs.join(name))
            .unwrap_or_else(|e| panic!("docs/{name} must exist: {e}"));
        let lines = text.lines().count();
        assert!(lines > 0, "{name} is empty");
        assert!(
            lines <= max_lines,
            "{name} is {lines} lines, over its {max_lines}-line budget"
        );
    }
    for retired in ["usage-patterns.md", "recipes.md"] {
        assert!(
            !docs.join(retired).exists(),
            "{retired} belongs to the graphs catalogue now"
        );
    }
    let index = fs::read_to_string(docs.join("index.md")).expect("index.md must exist");
    assert!(
        index.contains("https://graphs.univerlab.org/patterns"),
        "index.md must point at the graphs catalogue patterns page"
    );
}
