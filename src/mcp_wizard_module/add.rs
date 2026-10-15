use anyhow::Result;
use std::io::{self, BufRead};
use std::path::Path;

use crate::setup_module::Platform;

use super::parse::{infer_display_type, parse_kv_map, parse_pasted_servers, split_command_line};
use super::platform_config::{add_server_to_platforms, collect_all_platform_configs};
use super::remove::collect_all_server_names;
use super::{confirm, prompt_trimmed_text, show_updated_table, validate_required_input};
use super::{AddInputMode, AddServerInput, ServerTransport};

/// Ask how the server is described, collect its entries, preview them,
/// then inject each into every detected platform.
pub fn run_add(home: &Path, detected: &[&Platform]) -> Result<()> {
    println!();
    println!("  \x1b[1mAdd MCP Server\x1b[0m");
    println!("  ─────────────────────────────────────────────");

    let mode = prompt_add_input_mode()?;

    let mut pending: Vec<(AddServerInput, serde_json::Value)> = Vec::new();
    match mode {
        AddInputMode::Paste => {
            let Some(pasted) = prompt_paste_servers()? else {
                return Ok(());
            };
            for (name, entry) in pasted {
                let input = AddServerInput {
                    server_type: infer_display_type(&entry).to_string(),
                    name,
                    transport: ServerTransport::Pasted(entry),
                };
                let config = build_server_config(&input);
                pending.push((input, config));
            }
        }
        AddInputMode::LocalCommand => {
            let Some(input) = prompt_stdio_input()? else {
                return Ok(());
            };
            let config = build_server_config(&input);
            pending.push((input, config));
        }
        AddInputMode::RemoteUrl => {
            let Some(input) = prompt_url_input()? else {
                return Ok(());
            };
            let config = build_server_config(&input);
            pending.push((input, config));
        }
    }

    let all_configs = collect_all_platform_configs(home, detected);
    let existing = collect_all_server_names(&all_configs);

    for (input, config) in &pending {
        let name = &input.name;
        if existing.contains(name) {
            println!("  \x1b[1m{name}\x1b[0m \x1b[33m(replaces existing)\x1b[0m");
        } else {
            println!("  \x1b[1m{name}\x1b[0m");
        }
        println!("{}", serde_json::to_string_pretty(config)?);
    }

    if !confirm(&format!("Install on {} platform(s)?", detected.len()))? {
        println!("  Cancelled.");
        return Ok(());
    }

    for (input, config) in &pending {
        let name = &input.name;
        println!();
        println!(
            "  Installing \x1b[1m{name}\x1b[0m ({}) …",
            input.server_type
        );
        let (ok, fail) = add_server_to_platforms(home, detected, name, config);
        if fail == 0 {
            println!("  \x1b[32m✓\x1b[0m '{name}' added to {ok} platform(s).");
        } else {
            println!("  \x1b[33m⚠\x1b[0m '{name}' added to {ok}, failed on {fail} platform(s).");
        }
    }

    println!();
    show_updated_table(home, detected);
    Ok(())
}

pub(super) fn prompt_add_input_mode() -> Result<AddInputMode> {
    let choice = inquire::Select::new(
        "How do you want to describe the server?",
        vec![
            "Paste JSON — the snippet from the server's README",
            "Local command (stdio)",
            "Remote URL (http)",
        ],
    )
    .with_help_message("↑↓ navigate | Enter select | Esc cancel")
    .prompt()
    .map_err(super::cancelled_prompt)?;

    Ok(match choice {
        c if c.starts_with("Paste") => AddInputMode::Paste,
        c if c.starts_with("Local") => AddInputMode::LocalCommand,
        c if c.starts_with("Remote") => AddInputMode::RemoteUrl,
        _ => AddInputMode::Paste,
    })
}

pub(super) fn read_pasted_text() -> Result<String> {
    println!("Paste the JSON, then press Enter on an empty line (Ctrl-D also ends):");
    let stdin = io::stdin();
    let mut lines = Vec::new();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            break;
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

pub(super) fn prompt_paste_servers() -> Result<Option<Vec<(String, serde_json::Value)>>> {
    let text = read_pasted_text()?;
    match parse_pasted_servers(&text) {
        Ok(servers) => Ok(Some(servers)),
        Err(error) => {
            println!("  \x1b[31m✗\x1b[0m {error}");
            Ok(None)
        }
    }
}

pub(super) fn prompt_stdio_input() -> Result<Option<AddServerInput>> {
    let name = prompt_trimmed_text("Server name (e.g. \"github\"):")?;
    if !validate_required_input(&name, "Server name is required.") {
        return Ok(None);
    }

    let line = prompt_trimmed_text(
        "Command with its arguments (e.g. \"uvx git+https://github.com/org/server\"):",
    )?;
    if !validate_required_input(&line, "Command is required.") {
        return Ok(None);
    }
    let (command, args) = match split_command_line(&line) {
        Ok(parts) => parts,
        Err(error) => {
            println!("  \x1b[31m✗\x1b[0m {error}");
            return Ok(None);
        }
    };

    let env_text = prompt_trimmed_text(
        "Environment variables (KEY=VALUE separated by spaces, empty for none):",
    )?;
    let env = match parse_kv_map(&env_text) {
        Ok(map) => map,
        Err(error) => {
            println!("  \x1b[31m✗\x1b[0m {error}");
            return Ok(None);
        }
    };

    Ok(Some(AddServerInput {
        name,
        server_type: "stdio".to_string(),
        transport: ServerTransport::Command { command, args, env },
    }))
}

pub(super) fn prompt_url_input() -> Result<Option<AddServerInput>> {
    let name = prompt_trimmed_text("Server name (e.g. \"github\"):")?;
    if !validate_required_input(&name, "Server name is required.") {
        return Ok(None);
    }

    let url = prompt_trimmed_text("Server URL (e.g. \"https://example.com/mcp\"):")?;
    if !validate_required_input(&url, "Server URL is required.") {
        return Ok(None);
    }

    let headers_text = prompt_trimmed_text(
        "HTTP headers (Name=Value separated by spaces, quote values with spaces, empty for none):",
    )?;
    let headers = match parse_kv_map(&headers_text) {
        Ok(map) => map,
        Err(error) => {
            println!("  \x1b[31m✗\x1b[0m {error}");
            return Ok(None);
        }
    };

    Ok(Some(AddServerInput {
        name,
        server_type: "http".to_string(),
        transport: ServerTransport::Url { url, headers },
    }))
}

pub fn build_server_config(input: &AddServerInput) -> serde_json::Value {
    match &input.transport {
        ServerTransport::Url { url, headers } => {
            let mut config = serde_json::json!({
                "type": "http",
                "url": url,
            });
            if !headers.is_empty() {
                config["headers"] = serde_json::json!(headers);
            }
            config
        }
        ServerTransport::Command { command, args, env } => {
            let mut config = serde_json::json!({
                "type": "stdio",
                "command": command,
                "args": args,
            });
            if !env.is_empty() {
                config["env"] = serde_json::json!(env);
            }
            config
        }
        ServerTransport::Pasted(entry) => entry.clone(),
    }
}
