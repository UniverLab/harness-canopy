//! Tests for `docs/first-graph.md`: the guide a new user follows from a
//! fresh machine to a first graph run must stay accurate. The page carries
//! one copy-paste flow that works with an empty home directory and no AI
//! CLI, so these tests pin three things: the front matter has the keys the
//! docs site needs, every `canopy` line in a `<!-- test -->` block names a
//! real subcommand chain with real flags, and the page's own JSON document
//! passes the same validator `canopy graph validate` runs.
//!
//! The clap check only proves the commands exist — it never spawns a
//! `canopy` binary (starting one would SIGTERM this graph's own runs).

use clap::CommandFactory;

use super::Cli;

/// The guide under test.
fn guide_text() -> String {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join("docs/first-graph.md"))
        .expect("docs/first-graph.md must be readable")
}

/// Fenced block bodies whose opening fence is ` ```<lang>` on the line
/// right after a `<!-- test -->` marker.
fn marked_blocks(text: &str, lang: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "<!-- test -->" {
            continue;
        }
        for line in lines.by_ref() {
            if line.trim().is_empty() {
                continue;
            }
            let fence = format!("```{lang}");
            assert!(
                line.trim_start().starts_with(&fence),
                "line after `<!-- test -->` must open a ```{lang} fence, got: {line}"
            );
            let mut body = String::new();
            for line in lines.by_ref() {
                if line.trim_start().starts_with("```") {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            blocks.push(body);
            break;
        }
    }
    blocks
}

/// Split a shell line into tokens, honoring double quotes so names like
/// `"first-graph"` stay one token.
fn split_tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut in_token = false;
    for ch in line.chars() {
        if in_quote {
            if ch == '"' {
                in_quote = false;
            } else {
                current.push(ch);
            }
        } else if ch == '"' {
            in_quote = true;
            in_token = true;
        } else if ch.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut current));
                in_token = false;
            }
        } else {
            current.push(ch);
            in_token = true;
        }
    }
    if in_token {
        tokens.push(current);
    }
    tokens
}

/// Walk one `canopy ...` line down the real clap tree: every leading token
/// must name an existing subcommand, and every `--flag` must be defined on
/// the command it is passed to. Anything else is a positional value. Returns
/// the chain of subcommands walked so the caller can assert the guide really
/// names each required chain — stopping at a positional would otherwise let a
/// removed subcommand pass unnoticed (the chain would just be shorter).
fn check_canopy_line(line: &str) -> Vec<String> {
    let tokens = split_tokens(line);
    assert!(
        tokens.first().is_some_and(|first| first == "canopy"),
        "checked line must start with `canopy`, got: {line}"
    );
    let mut cmd = Cli::command();
    let mut chain = Vec::new();
    let mut index = 1;
    while index < tokens.len() {
        let Some(next) = cmd.find_subcommand(&tokens[index]).cloned() else {
            break;
        };
        cmd = next;
        chain.push(tokens[index].clone());
        index += 1;
    }
    assert!(
        !chain.is_empty(),
        "`{line}` names no `canopy` subcommand; the guide must only use real ones"
    );
    for token in &tokens[index..] {
        if let Some(long) = token.strip_prefix("--") {
            let name = long.split('=').next().unwrap_or(long);
            let defined = cmd
                .get_arguments()
                .any(|arg| arg.get_long().is_some_and(|long| long == name));
            assert!(
                defined,
                "`{line}` passes `--{name}`, which `{}` does not define",
                cmd.get_name()
            );
        } else if token.starts_with('-') && token.len() > 1 {
            let short = token.chars().nth(1).expect("short flag has a char");
            let defined = cmd
                .get_arguments()
                .any(|arg| arg.get_short().is_some_and(|s| s == short));
            assert!(
                defined,
                "`{line}` passes `-{short}`, which `{}` does not define",
                cmd.get_name()
            );
        }
    }
    chain
}

/// The page's own JSON document, through the validator behind
/// `canopy graph validate` (no database, no daemon).
fn guide_json(text: &str) -> serde_json::Value {
    let mut blocks = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("```json") {
            continue;
        }
        let mut body = String::new();
        for line in lines.by_ref() {
            if line.trim_start().starts_with("```") {
                break;
            }
            body.push_str(line);
            body.push('\n');
        }
        blocks.push(body);
    }
    assert_eq!(
        blocks.len(),
        1,
        "first-graph.md must carry exactly one ```json document, found {}",
        blocks.len()
    );
    serde_json::from_str(&blocks[0]).expect("the guide's JSON must parse")
}

#[test]
fn front_matter_has_site_keys() {
    let text = guide_text();
    assert!(
        text.starts_with("---\n"),
        "the guide must open with front matter"
    );
    let end = text[4..]
        .find("\n---")
        .expect("the guide's front matter must close");
    let mut keys = std::collections::HashSet::new();
    for line in text[4..end + 4].lines() {
        if let Some((key, value)) = line.split_once(':') {
            keys.insert(key.trim().to_string());
            assert!(
                !value.trim().is_empty(),
                "front matter key `{}` must have a value",
                key.trim()
            );
        }
    }
    for required in ["title", "description", "order"] {
        assert!(
            keys.contains(required),
            "front matter must carry `{required}` like the other docs pages"
        );
    }
}

#[test]
fn marked_canopy_lines_name_real_commands() {
    let text = guide_text();
    let blocks = marked_blocks(&text, "bash");
    assert!(
        !blocks.is_empty(),
        "at least one ```bash block must follow a `<!-- test -->` marker"
    );
    let mut chains: Vec<Vec<String>> = Vec::new();
    for block in &blocks {
        for line in block.lines() {
            let line = line.trim();
            if line.starts_with("canopy") {
                chains.push(check_canopy_line(line));
            }
        }
    }
    assert!(
        chains.len() >= 5,
        "the guide's flow needs daemon start, validate, import, run and log ({} checked)",
        chains.len()
    );
    // Walk as far as the real clap tree allows and demand the full documented
    // chain, not just a real prefix: otherwise deleting `daemon start` or
    // `graph log` would leave a shorter (still passing) chain behind.
    for expected in [
        &["daemon", "start"][..],
        &["graph", "validate"][..],
        &["graph", "import"][..],
        &["graph", "run"][..],
        &["graph", "log"][..],
    ] {
        assert!(
            chains.iter().any(|chain| chain
                .iter()
                .map(String::as_str)
                .eq(expected.iter().copied())),
            "the guide must document the `{expected:?}` subcommand chain; walked: {chains:?}"
        );
    }
}

#[test]
fn guide_json_passes_graph_validate() {
    let text = guide_text();
    let document = guide_json(&text);
    let report = crate::daemon::graph_validate::validate_document_value(&document);
    assert_eq!(
        report.get("ok"),
        Some(&serde_json::Value::Bool(true)),
        "the guide's JSON must validate cleanly: {report}"
    );
}
