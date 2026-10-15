use anyhow::Result;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::Path;

use crate::setup_module::Platform;
use crate::shared::banner;

use super::remove::collect_all_server_names;
use super::PlatformConfigs;

pub(super) fn clear_screen() -> Result<()> {
    print!("\x1b[2J\x1b[H");
    io::stdout().flush()?;
    Ok(())
}

/// Human-readable source the registry fetch used: `local: <path>` when a
/// local registry directory is set (`--local-registry`), `github` otherwise.
/// Pure so it can be unit-tested without global state.
pub fn registry_source_label(local: Option<&Path>) -> String {
    match local {
        Some(path) => format!("local: {}", path.display()),
        None => "github".to_string(),
    }
}

pub(super) fn print_mcp_banner() {
    banner::print_banner_with_gradient("Agent Hub — MCP Manager");
    // Removed duplicate line - banner function already prints the separator line
}

// ── Matrix table / cards ────────────────────────────────────────────────────

/// Fallback terminal width used when the real size can't be detected (e.g.
/// non-interactive test runs), matching the convention used across the TUI.
pub const DEFAULT_TERM_WIDTH: usize = 120;

/// Print platforms-vs-MCPs as a matrix table (columns = platforms) when it
/// fits the terminal width, or as one card per platform otherwise. The matrix
/// layout grows a column per platform, so past a handful of platforms it
/// wraps and misaligns; the card view scales to any number of platforms.
pub fn print_mcp_table(detected: &[&Platform], all_configs: &PlatformConfigs) {
    let term_width = ratatui::crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(DEFAULT_TERM_WIDTH);
    print!("{}", render_mcp_table(detected, all_configs, term_width));
}

/// `term_width` is passed in rather than read here so the layout choice is a
/// function of its arguments alone. Reading the real terminal inside made the
/// tests depend on the width of whatever terminal happened to run them: the
/// eight-platform case needs 159 columns, so it rendered as cards under a
/// narrow terminal and as a matrix under a wide one, and the assertion that
/// no line exceeds 120 columns failed only on wide terminals.
pub fn render_mcp_table(
    detected: &[&Platform],
    all_configs: &PlatformConfigs,
    term_width: usize,
) -> String {
    let all_servers = collect_all_server_names(all_configs);
    if all_servers.is_empty() {
        return "  \x1b[90mNo MCP servers configured.\x1b[0m\n\n".to_string();
    }

    let name_col = all_servers
        .iter()
        .map(|server| server.len())
        .max()
        .unwrap_or(6)
        .max(6);
    let plat_col = detected
        .iter()
        .map(|platform| platform.name.len())
        .max()
        .unwrap_or(4)
        .max(4);

    let matrix_width = 2 + name_col + detected.len() * (plat_col + 2);

    if matrix_width > term_width.max(DEFAULT_TERM_WIDTH) {
        render_mcp_cards(&all_servers, detected, all_configs, name_col)
    } else {
        render_mcp_matrix(&all_servers, detected, all_configs, name_col, plat_col)
    }
}

pub fn render_mcp_matrix(
    all_servers: &BTreeSet<String>,
    detected: &[&Platform],
    all_configs: &PlatformConfigs,
    name_col: usize,
    plat_col: usize,
) -> String {
    let mut out = String::new();
    write!(out, "  {:<name_col$}", "Server").unwrap();
    for platform in detected {
        write!(out, "  {:>plat_col$}", platform.name).unwrap();
    }
    out.push('\n');

    let total_w = name_col + detected.len() * (plat_col + 2);
    writeln!(out, "  {:─<total_w$}", "").unwrap();

    for server in all_servers {
        write!(out, "  {server:<name_col$}").unwrap();
        for platform in detected {
            let has_server = all_configs
                .get(&platform.name)
                .is_some_and(|servers| servers.contains_key(server));
            let pad = plat_col.saturating_sub(1);
            write!(
                out,
                "  {}{}",
                " ".repeat(pad),
                server_presence_icon(has_server)
            )
            .unwrap();
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

/// One section per platform listing every known MCP server with its ✓/✗,
/// used instead of the matrix when there are too many platforms to fit as
/// columns.
pub fn render_mcp_cards(
    all_servers: &BTreeSet<String>,
    detected: &[&Platform],
    all_configs: &PlatformConfigs,
    name_col: usize,
) -> String {
    let mut out = String::new();
    for platform in detected {
        writeln!(out, "  \x1b[1m{}\x1b[0m", platform.name).unwrap();
        let empty = std::collections::BTreeMap::new();
        let servers = all_configs.get(&platform.name).unwrap_or(&empty);
        for server in all_servers {
            let has_server = servers.contains_key(server);
            writeln!(
                out,
                "    {server:<name_col$}  {}",
                server_presence_icon(has_server)
            )
            .unwrap();
        }
        out.push('\n');
    }
    out
}

pub fn server_presence_icon(has_server: bool) -> &'static str {
    if has_server {
        "\x1b[32m ✓\x1b[0m"
    } else {
        "\x1b[31m ✗\x1b[0m"
    }
}
