//! Docs test for `docs/network-and-privacy.md`.
//!
//! The page lists every outbound connection canopy makes in one table whose
//! "how to turn it off" cell takes exactly one of three forms: a registry
//! key in backticks, a `canopy ...` command in backticks (or the plain
//! sentence `only runs when you type` with one), or the plain words
//! `no switch`. This module pins those forms so a later edit cannot silently
//! widen them.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn page_text() -> String {
    let path = repo_root().join("docs/network-and-privacy.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("network-and-privacy.md must be readable: {e}"))
}

fn front_matter(text: &str) -> String {
    let rest = text
        .strip_prefix("---\n")
        .expect("page must open with a front matter delimiter");
    let end = rest
        .find("\n---")
        .expect("front matter must have a closing delimiter");
    rest[..end].to_string()
}

fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .strip_prefix('|')
        .and_then(|s| s.strip_suffix('|'))
        .expect("table row must start and end with `|`")
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

/// The table's data rows: the lines after the header naming
/// "how to turn it off", minus the separator row.
fn table_rows(text: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let header = lines
        .iter()
        .position(|line| line.contains("how to turn it off"))
        .expect("page must have a table with a `how to turn it off` column");
    lines[header + 2..]
        .iter()
        .take_while(|line| line.trim_start().starts_with('|'))
        .map(|line| split_row(line))
        .collect()
}

fn column_index(header_line: &str, name: &str) -> usize {
    split_row(header_line)
        .iter()
        .position(|cell| cell == name)
        .unwrap_or_else(|| panic!("table header must have a `{name}` column"))
}

fn header_line(text: &str) -> &str {
    text.lines()
        .find(|line| line.contains("how to turn it off"))
        .expect("page must have a table with a `how to turn it off` column")
}

/// Backticked tokens in a cell, in order.
fn backticked(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .collect()
}

fn closing_section(text: &str) -> &str {
    let marker = "## Connections with no switch";
    let start = text
        .find(marker)
        .expect("page must end with a `Connections with no switch` section");
    &text[start..]
}

#[test]
fn front_matter_has_title_description_and_order() {
    let fm = front_matter(&page_text());
    for key in ["title", "description", "order"] {
        let value = fm
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .unwrap_or_else(|| panic!("front matter must have `{key}`"));
        assert!(
            !value.trim().is_empty(),
            "front matter `{key}` must not be empty"
        );
    }
}

#[test]
fn off_cells_match_one_of_the_three_forms() {
    let text = page_text();
    let header = header_line(&text);
    let off = column_index(header, "how to turn it off");
    let rows = table_rows(&text);
    // The spec inventory at the time of writing.
    assert_eq!(
        rows.len(),
        16,
        "network table must list the 16 inventory rows"
    );
    for row in &rows {
        let cell = row[off].trim();
        if cell == "no switch" {
            continue;
        }
        let tokens = backticked(cell);
        assert!(
            !tokens.is_empty(),
            "off cell must hold a backticked key or command, or be `no switch`: {cell}"
        );
        for token in &tokens {
            if token.starts_with("canopy ") {
                continue;
            }
            assert!(
                super::find(token).is_some(),
                "off cell token `{token}` must be a registry key"
            );
        }
    }
}

#[test]
fn closing_list_matches_the_no_switch_rows() {
    let text = page_text();
    let header = header_line(&text);
    let dest = column_index(header, "destination");
    let off = column_index(header, "how to turn it off");
    let rows = table_rows(&text);
    let unswitched: Vec<String> = rows
        .iter()
        .filter(|row| row[off].trim() == "no switch")
        .map(|row| row[dest].replace('`', ""))
        .collect();
    assert!(
        !unswitched.is_empty(),
        "at least one row must have `no switch`"
    );
    let section = closing_section(&text);
    let bullets: Vec<&str> = section
        .lines()
        .filter(|line| line.trim_start().starts_with("- "))
        .collect();
    assert_eq!(
        bullets.len(),
        unswitched.len(),
        "closing list must name exactly the `no switch` rows"
    );
    for dest in &unswitched {
        assert!(
            section.contains(dest),
            "closing list must name the `no switch` row: {dest}"
        );
    }
}

#[test]
fn readme_and_index_link_the_page() {
    let readme =
        std::fs::read_to_string(repo_root().join("README.md")).expect("README.md must be readable");
    assert!(
        readme.contains("docs/network-and-privacy.md"),
        "README.md must link the network and privacy page"
    );
    let index = std::fs::read_to_string(repo_root().join("docs/index.md"))
        .expect("docs/index.md must be readable");
    assert!(
        index.contains("network-and-privacy.md"),
        "docs/index.md must link the network and privacy page"
    );
}
