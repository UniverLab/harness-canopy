use super::{visible_tool_list, HIDDEN_DEPRECATED_TOOLS};
use std::collections::BTreeSet;
use std::fs;

fn canonical() -> BTreeSet<String> {
    visible_tool_list()
        .into_iter()
        .map(|t| t.name.as_ref().to_owned())
        .collect::<BTreeSet<String>>()
}

fn first_cell_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') || !trimmed.ends_with('|') {
            continue;
        }
        let first_cell = trimmed.split('|').nth(1).unwrap_or("");
        let bytes = first_cell.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == b'`' {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'`' {
                    let name = &first_cell[start..i];
                    if !name.is_empty()
                        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    {
                        names.insert(name.to_string());
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
    }
    names
}

fn section(text: &str, head: &str) -> String {
    let head_trim = head.trim();
    let mut result = String::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let trimmed_start = line.trim_start();
        if !in_section {
            if trimmed.starts_with(head_trim) {
                in_section = true;
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }
        if trimmed_start.starts_with("## ") || trimmed_start.starts_with("---") {
            break;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

fn heading_total(sec: &str) -> usize {
    for line in sec.lines() {
        let trimmed = line.trim();
        if trimmed.contains("MCP Tools") && trimmed.contains('(') && trimmed.contains(')') {
            if let Some(start) = trimmed.find('(') {
                if let Some(end) = trimmed[start + 1..].find(')') {
                    let num = &trimmed[start + 1..start + 1 + end];
                    if let Ok(v) = num.parse::<usize>() {
                        return v;
                    }
                }
            }
        }
    }
    0
}

#[test]
fn readme_and_docs_match_registered() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let readme_path = format!("{}/README.md", manifest_dir);
    let docs_path = format!("{}/docs/mcp-tools.md", manifest_dir);
    let readme = fs::read_to_string(&readme_path).expect("Failed to read README.md");
    let docs = fs::read_to_string(&docs_path).expect("Failed to read docs/mcp-tools.md");
    let readme_section = section(&readme, "## MCP Tools");
    let readme_names = first_cell_names(&readme_section);
    let canonical_names = canonical();
    assert_eq!(
        readme_names,
        canonical_names,
        "README MCP tools table mismatch. missing: {:?}, extra: {:?}",
        canonical_names.difference(&readme_names),
        readme_names.difference(&canonical_names)
    );
    let mut docs_names = first_cell_names(&docs);
    for h in HIDDEN_DEPRECATED_TOOLS {
        docs_names.remove(*h);
    }
    assert_eq!(
        docs_names,
        canonical_names,
        "docs/mcp-tools.md mismatch. missing: {:?}, extra: {:?}",
        canonical_names.difference(&docs_names),
        docs_names.difference(&canonical_names)
    );
    assert_eq!(
        heading_total(&readme_section),
        canonical_names.len(),
        "README heading total mismatch"
    );
}
