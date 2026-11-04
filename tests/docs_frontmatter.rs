//! Documentation frontmatter check: the YAML frontmatter of every `docs/*.md`
//! must parse. Univerlab builds its `/canopy/docs/` pages from this directory,
//! so one invalid frontmatter line breaks the site build.
//!
//! The check shells out to `python3` with `PyYAML`, which is preinstalled on the
//! GitHub runners and in this environment; no new dependency is added.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The frontmatter body between the `---` delimiters, or `None` when the file
/// has no frontmatter. The closing delimiter is excluded: handing it to
/// `yaml.safe_load` raises "expected a single document in the stream" instead
/// of validating the frontmatter itself.
fn frontmatter(text: &str) -> Option<&str> {
    if !text.starts_with("---\n") {
        return None;
    }
    let end = text[4..].find("\n---")? + 4;
    Some(&text[4..end])
}

/// Parse `source` as YAML, returning the parser's diagnosis on failure.
fn yaml_ok(source: &str) -> Result<(), String> {
    let mut child = Command::new("python3")
        .args(["-c", "import sys, yaml; yaml.safe_load(sys.stdin)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 with PyYAML is required for the docs frontmatter check");
    child
        .stdin
        .as_mut()
        .expect("python3 stdin must be piped")
        .write_all(source.as_bytes())
        .expect("writing the frontmatter to python3 must succeed");
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .expect("waiting for python3 must succeed");
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // PyYAML's traceback ends with the unindented `Exception: message` line
    // followed by an indented location line; the diagnosis is the exception.
    let last = stderr
        .rsplit('\n')
        .map(str::trim_end)
        .find(|line| !line.trim().is_empty() && !line.starts_with(char::is_whitespace))
        .or_else(|| stderr.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("python3 failed with no message")
        .to_string();
    Err(last)
}

#[test]
fn docs_frontmatter_is_valid_yaml() {
    let docs = repo_root().join("docs");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&docs)
        .expect("docs/ must exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no docs/*.md files found in {}",
        docs.display()
    );
    let mut checked = 0usize;
    for file in &files {
        let name = file.file_name().expect("file has a name").to_string_lossy();
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("docs/{name} must be readable: {e}"));
        let opens = text.starts_with("---\n");
        let Some(fm) = frontmatter(&text) else {
            // An opener with no closing delimiter is broken frontmatter, not a
            // file without frontmatter; skipping it would pass vacuously.
            assert!(!opens, "docs/{name}: frontmatter opened but never closed");
            continue;
        };
        checked += 1;
        if let Err(err) = yaml_ok(fm) {
            panic!("docs/{name}: {err}");
        }
    }
    // The check must actually have parsed something: an extractor that returns
    // None for every file would otherwise let every doc through unchecked.
    assert!(checked > 0, "no docs/*.md frontmatter was checked");
}

#[test]
fn invalid_frontmatter_fails_the_check() {
    // The exact line that broke the univerlab build before it was quoted.
    let fm = "description: Reusable node documents: install, list, add, remove.\n";
    let err = yaml_ok(fm).expect_err("the unquoted `: ` frontmatter must fail the check");
    assert!(
        err.contains("mapping values are not allowed here"),
        "unexpected diagnosis: {err}"
    );
}
