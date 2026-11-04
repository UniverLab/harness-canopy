//! CLI handlers for `canopy shelf` subcommands — the session shelf files
//! agents send with `session_send_file`.
//!
//! Both subcommands go through the running daemon over the session-protocol
//! WebSocket (`sessions::client`), never by writing the shelf database or
//! bytes themselves: the daemon is the only shelf writer, and the CLI stays
//! a client of it — the same rule `cli_daemon` states for graph/spec.
//!
//! `shelf list` additionally reads session ids from the local database
//! (read-only, via `Database::new_safe`) so it can ask the daemon for every
//! session's shelf, not just the host shelf.

use std::collections::HashSet;

use anyhow::{bail, Context as _, Result};
use chrono::{DateTime, Local};
use clap::Subcommand;

use crate::daemon::bridge::resolve_bridge_port;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::sessions::client::{read_token, ws_url, HubClient};
use crate::sessions::protocol::ShelfItem;
use crate::sessions::shelf::{is_file_id, shelf_file_path};

#[derive(Subcommand, Debug)]
pub(crate) enum ShelfAction {
    /// List every shelf file known to canopy (the host shelf plus every
    /// session's shelf), newest first, served by the daemon.
    List {
        /// Print the items as JSON (with their file ids) instead of
        /// human-readable lines.
        #[arg(long)]
        json: bool,
    },
    /// Open one shelf file with the system viewer, going through the daemon.
    Open {
        /// The shelf file id: 16 lowercase hex characters, as listed by
        /// `canopy shelf list --json`.
        file_id: String,
    },
}

pub async fn handle_shelf_action(action: ShelfAction, port_override: Option<u16>) -> Result<()> {
    match action {
        ShelfAction::List { json } => handle_list(port_override, json),
        ShelfAction::Open { file_id } => handle_open(port_override, &file_id),
    }
}

/// One authenticated control connection to the daemon's session hub. An
/// unreachable daemon is reported the same way every daemon-calling CLI
/// reports it, never as an empty shelf or a missing file.
fn connect(port_override: Option<u16>) -> Result<HubClient> {
    let port = resolve_bridge_port(port_override);
    let data_dir = crate::ensure_data_dir()?;
    let token = read_token(&data_dir).context("canopy: could not read the daemon token")?;
    HubClient::connect(&ws_url(port), &token).with_context(|| {
        format!(
            "canopy: could not reach the daemon on port {port}. Check `canopy daemon status`, \
             or start it with `canopy daemon start`."
        )
    })
}

fn handle_list(port_override: Option<u16>, json: bool) -> Result<()> {
    let client = connect(port_override)?;

    // The host shelf must be servable by a daemon that speaks the shelf
    // protocol; a failure here is the daemon's problem, not "no files".
    let mut items =
        request_shelf(&client, None).context("canopy: the daemon could not list the host shelf")?;

    // Session ids come from the local database, read-only — the CLI only
    // learns which shelves exist; the daemon serves their contents. A
    // session whose shelf request fails (a dropped connection, a timeout)
    // is skipped, not fatal: one broken shelf must not hide every other
    // shelf (an unknown id simply answers an empty shelf).
    for session_id in session_ids_from_db()? {
        match request_shelf(&client, Some(&session_id)) {
            Ok(mut more) => items.append(&mut more),
            Err(err) => tracing::debug!("skipping shelf of session {session_id}: {err}"),
        }
    }

    let items = merge_items(items);
    if json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("No shelf files.");
        return Ok(());
    }
    for line in render_shelf_items(&items) {
        println!("{line}");
    }
    Ok(())
}

fn handle_open(port_override: Option<u16>, file_id: &str) -> Result<()> {
    let file_id = file_id.trim();
    if !is_file_id(file_id) {
        bail!("'{file_id}' is not a shelf file id — expected 16 lowercase hex characters");
    }
    let client = connect(port_override)?;
    // The whole fetch goes through the daemon: an unknown id is the hub's
    // `not_found`, reported as "no such shelf file" below. The bytes come
    // back over the local socket, but the viewer needs a path — and this
    // machine already holds the same bytes — so the local shelf path is
    // opened directly and the fetched copy is dropped.
    let _bytes = client
        .shelf_get(file_id)
        .map_err(|err| open_request_error(err, file_id))?;
    let path = shelf_file_path(file_id)
        .ok_or_else(|| anyhow::anyhow!("invalid shelf file id '{file_id}'"))?;
    crate::shared::open::open_path(&path)
        .with_context(|| format!("could not open shelf file {}", path.display()))?;
    Ok(())
}

/// `not_found` from the hub is "no such shelf file"; every other failure
/// (timeout, closed connection) keeps its own message so it is never
/// mistaken for a missing file.
fn open_request_error(err: anyhow::Error, file_id: &str) -> anyhow::Error {
    if err.to_string().contains("not_found") {
        anyhow::anyhow!("no shelf file '{file_id}'")
    } else {
        err
    }
}

/// One shelf's items over the hub; `None` addresses the host shelf.
fn request_shelf(client: &HubClient, session_id: Option<&str>) -> Result<Vec<ShelfItem>> {
    client.shelf_list(session_id)
}

/// Session ids the shelf may have files for, read from the local database
/// without touching it (the same read-only open `sandbox list` uses): every
/// live session (a live session can gain a file mid-run) plus every session
/// named in a shelf row — a finished session keeps its files for 7 days
/// after it drops out of the live set, and its id survives in its rows.
fn session_ids_from_db() -> Result<Vec<String>> {
    let data_dir = crate::ensure_data_dir()?;
    let db = Database::new_safe(&database_path(&data_dir), &data_dir)?;
    let mut ids: HashSet<String> = db.live_session_ids()?;
    for row in db.all_shelf_files_oldest_first()? {
        if let Some(session_id) = row.session_id {
            ids.insert(session_id);
        }
    }
    let mut ids: Vec<String> = ids.into_iter().collect();
    ids.sort();
    Ok(ids)
}

/// Merge every shelf's items into one listing: one copy per file id,
/// newest first. `created_at` is written as UTC RFC 3339 everywhere, so
/// lexicographic order is chronological order.
fn merge_items(mut items: Vec<ShelfItem>) -> Vec<ShelfItem> {
    let mut seen: HashSet<String> = HashSet::new();
    items.retain(|item| seen.insert(item.file_id.clone()));
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    items
}

/// Human-readable lines for `shelf list`: one per item, newest first.
fn render_shelf_items(items: &[ShelfItem]) -> Vec<String> {
    items.iter().map(render_shelf_item).collect()
}

/// One item: local `HH:MM`, sender, name, human size, caption after an em
/// dash when present.
fn render_shelf_item(item: &ShelfItem) -> String {
    let mut line = format!(
        "{}  {}  {} ({})",
        local_hhmm(&item.created_at),
        item.from_label,
        item.name,
        human_size(item.size),
    );
    if let Some(caption) = item.caption.as_deref().filter(|c| !c.is_empty()) {
        line.push_str(&format!(" — {caption}"));
    }
    line
}

/// The item's `created_at` as local `HH:MM`; the raw string when it is not
/// an RFC 3339 instant.
fn local_hhmm(created_at: &str) -> String {
    DateTime::parse_from_rfc3339(created_at)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| created_at.to_string())
}

/// `123 B`, `1.4 KiB`, `3.2 MiB` — the shelf's 20 MiB per-file cap keeps
/// this short.
fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes < 1024.0 {
        format!("{bytes:.0} B")
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KiB", bytes / 1024.0)
    } else {
        format!("{:.1} MiB", bytes / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: ShelfAction,
    }

    fn item(file_id: &str, created_at: &str) -> ShelfItem {
        ShelfItem {
            file_id: file_id.to_string(),
            session_id: None,
            name: "diagram.png".to_string(),
            mime: "image/png".to_string(),
            size: 1536,
            caption: Some("the flow".to_string()),
            from_label: "alice".to_string(),
            created_at: created_at.to_string(),
        }
    }

    #[test]
    fn list_parses_json_flag() {
        let cli = TestCli::try_parse_from(["test", "list", "--json"]).expect("should parse");
        match cli.action {
            ShelfAction::List { json } => assert!(json),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn list_parses_without_json_flag() {
        let cli = TestCli::try_parse_from(["test", "list"]).expect("should parse");
        match cli.action {
            ShelfAction::List { json } => assert!(!json),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn open_parses_file_id() {
        let cli =
            TestCli::try_parse_from(["test", "open", "deadbeefdeadbeef"]).expect("should parse");
        match cli.action {
            ShelfAction::Open { file_id } => assert_eq!(file_id, "deadbeefdeadbeef"),
            _ => panic!("expected Open"),
        }
    }

    #[test]
    fn render_shelf_item_shows_from_name_size_and_caption() {
        let line = render_shelf_item(&item("deadbeefdeadbeef", "2026-10-27T14:03:00Z"));
        let (time, rest) = line.split_once("  ").expect("time column");
        assert!(
            time.chars().count() == 5 && time.contains(':'),
            "expected HH:MM local time, got '{time}'"
        );
        assert!(rest.starts_with("alice  diagram.png (1.5 KiB)"), "{line}");
        assert!(line.ends_with(" — the flow"), "{line}");
    }

    #[test]
    fn render_shelf_item_falls_back_to_raw_time_and_skips_empty_caption() {
        let mut weird = item("deadbeefdeadbeef", "not-a-date");
        weird.caption = Some(String::new());
        weird.size = 512;
        let line = render_shelf_item(&weird);
        assert!(line.starts_with("not-a-date  alice"), "{line}");
        assert!(line.contains("(512 B)"), "{line}");
        assert!(!line.contains('—'), "{line}");
    }

    #[test]
    fn merge_items_dedupes_by_file_id_and_sorts_newest_first() {
        let merged = merge_items(vec![
            item("aaaaaaaaaaaaaaaa", "2026-10-27T10:00:00Z"),
            item("bbbbbbbbbbbbbbbb", "2026-10-27T12:00:00Z"),
            // Same file id arriving from another shelf: one copy survives.
            item("aaaaaaaaaaaaaaaa", "2026-10-27T10:00:00Z"),
        ]);
        let ids: Vec<&str> = merged.iter().map(|i| i.file_id.as_str()).collect();
        assert_eq!(ids, ["bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa"]);
    }

    #[test]
    fn human_size_covers_b_kib_and_mib() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(20 * 1024 * 1024), "20.0 MiB");
    }
}
