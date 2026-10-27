//! The F1 hub's Shelf tab: state, fetching and the selected item's actions.
//!
//! The shelf lists the files agents sent with `session_send_file`: the host
//! shelf plus one shelf per tracked session, merged, deduped and kept
//! newest first. Rendering lives in `tui/ui/dialogs/hub_shelf.rs`, the key
//! handling in `tui/event/hub.rs`.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::sessions::protocol::ShelfItem;

use super::types::App;

/// How long a `📎 …` toast stays on screen (also checked at draw time).
pub(crate) const TOAST_TTL: Duration = Duration::from_secs(5);

/// The hub's Shelf tab state.
#[derive(Debug, Default)]
pub(crate) struct ShelfState {
    /// Every shelf's files merged and deduped, newest first.
    pub items: Vec<ShelfItem>,
    pub sel: usize,
    /// Whether `d` is waiting for its `y`/`n`.
    pub delete_confirm: bool,
    pub toast: Option<ShelfToast>,
}

/// A transient `📎 …` line after an arrival or a failed open.
#[derive(Debug, Clone)]
pub(crate) struct ShelfToast {
    pub text: String,
    pub at: Instant,
}

impl App {
    /// Re-list every shelf the TUI tracks — the host shelf plus one per
    /// sidebar session — and replace the merged list with the answers. A
    /// shelf whose request fails (a dropped connection, a timeout) is
    /// skipped, and when no shelf answers at all the current list is kept,
    /// so one failed request never loses what was already fetched.
    pub(crate) fn refresh_shelf(&mut self) {
        let Some(client) = self.session_hub.client.clone() else {
            return;
        };
        let mut ids: Vec<Option<String>> = vec![None];
        for agent in self
            .interactive_agents
            .iter()
            .chain(self.terminal_agents.iter())
        {
            if !ids
                .iter()
                .any(|id| id.as_deref() == Some(agent.id.as_str()))
            {
                ids.push(Some(agent.id.clone()));
            }
        }

        let mut merged: Vec<ShelfItem> = Vec::new();
        let mut answered = false;
        for id in ids {
            match client.shelf_list(id.as_deref()) {
                Ok(items) => {
                    answered = true;
                    merged.extend(items);
                }
                Err(e) => tracing::warn!("Cannot list the shelf of {id:?}: {e}"),
            }
        }
        if !answered {
            return;
        }
        self.replace_shelf_items(merged);
    }

    /// A `shelf_added` broadcast: insert the new file, keep the list newest
    /// first and raise the arrival toast.
    pub(crate) fn note_shelf_added(&mut self, item: ShelfItem) {
        if self
            .hub
            .shelf
            .items
            .iter()
            .any(|known| known.file_id == item.file_id)
        {
            return;
        }
        let toast = ShelfToast {
            text: format!("📎 {} — {}", item.name, item.from_label),
            at: Instant::now(),
        };
        self.hub.shelf.items.insert(0, item);
        self.sort_shelf_items();
        self.clamp_shelf_selection();
        self.hub.shelf.toast = Some(toast);
    }

    /// A `shelf_removed` broadcast (also used by the confirmed local delete).
    pub(crate) fn note_shelf_removed(&mut self, file_id: &str) {
        self.hub.shelf.items.retain(|item| item.file_id != file_id);
        self.clamp_shelf_selection();
    }

    /// The `y`/Enter of the delete confirmation: drop the item locally and
    /// ask the host to remove the file (the host's `shelf_removed` broadcast
    /// repeats the drop for every other client). The local drop is
    /// optimistic — a failed host delete only logs, the row is already gone
    /// from this view.
    pub(crate) fn delete_shelf_selection(&mut self) {
        self.hub.shelf.delete_confirm = false;
        let Some(file_id) = self
            .hub
            .shelf
            .items
            .get(self.hub.shelf.sel)
            .map(|item| item.file_id.clone())
        else {
            return;
        };
        self.note_shelf_removed(&file_id);
        if let Some(client) = self.session_hub.client.as_ref() {
            if let Err(e) = client.shelf_delete(&file_id) {
                tracing::warn!("shelf delete of {file_id} failed: {e}");
            }
        }
    }

    /// ↑↓/jk over the items, wrapping at both ends like every TUI list.
    pub(crate) fn move_shelf_selection(&mut self, forward: bool) {
        self.hub.shelf.sel = crate::tui::selection::move_index(
            self.hub.shelf.sel,
            self.hub.shelf.items.len(),
            forward,
        );
    }

    /// Enter/Space on the selected file: open it with the system viewer; a
    /// failure (no opener, missing file, bad id) raises the toast.
    pub(crate) fn open_shelf_selection(&mut self) {
        let Some((name, file_id)) = self
            .hub
            .shelf
            .items
            .get(self.hub.shelf.sel)
            .map(|item| (item.name.clone(), item.file_id.clone()))
        else {
            return;
        };
        let Some(path) = crate::sessions::shelf::shelf_file_path(&file_id) else {
            self.shelf_toast(format!("📎 {name} — open failed"));
            return;
        };
        if let Err(e) = crate::shared::open::open_path(&path) {
            self.shelf_toast(format!("📎 {name} — open failed: {e}"));
        }
    }

    /// `c`: copy the selected file's absolute path and flash the COPIED badge.
    pub(crate) fn copy_shelf_selection_path(&mut self) {
        let Some(path) = self
            .hub
            .shelf
            .items
            .get(self.hub.shelf.sel)
            .and_then(|item| crate::sessions::shelf::shelf_file_path(&item.file_id))
        else {
            return;
        };
        crate::tui::clipboard::set_text(&path.display().to_string());
        self.show_copied = true;
        self.copied_at = Instant::now();
    }

    /// Raise a `📎 …` toast for [`TOAST_TTL`].
    pub(crate) fn shelf_toast(&mut self, text: String) {
        self.hub.shelf.toast = Some(ShelfToast {
            text,
            at: Instant::now(),
        });
    }

    /// Drop the shelf toast once its TTL passed (the draw side checks the
    /// age too, so a paused refresh loop never freezes a toast on screen).
    pub(crate) fn dismiss_shelf_toast(&mut self) {
        let expired = self
            .hub
            .shelf
            .toast
            .as_ref()
            .is_some_and(|toast| toast.at.elapsed() > TOAST_TTL);
        if expired {
            self.hub.shelf.toast = None;
        }
    }

    /// Replace the list with a freshly merged one, sorted and re-clamped.
    fn replace_shelf_items(&mut self, mut items: Vec<ShelfItem>) {
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let mut seen = HashSet::new();
        items.retain(|item| seen.insert(item.file_id.clone()));
        self.hub.shelf.items = items;
        self.clamp_shelf_selection();
    }

    /// Newest first (`created_at` DESC; stable, so equal timestamps keep
    /// the order they arrived in).
    fn sort_shelf_items(&mut self) {
        self.hub
            .shelf
            .items
            .sort_by(|a, b| b.created_at.cmp(&a.created_at));
    }

    /// Keep the selection inside the list after it shrinks.
    fn clamp_shelf_selection(&mut self) {
        self.hub.shelf.sel = self
            .hub
            .shelf
            .sel
            .min(self.hub.shelf.items.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::types::App;
    use std::sync::Arc;

    fn test_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn item(file_id: &str, name: &str, from: &str, created_at: &str) -> ShelfItem {
        ShelfItem {
            file_id: file_id.to_string(),
            session_id: None,
            name: name.to_string(),
            mime: "image/png".to_string(),
            size: 1024,
            caption: None,
            from_label: from.to_string(),
            created_at: created_at.to_string(),
        }
    }

    #[test]
    fn shelf_added_inserts_newest_first_and_raises_the_toast() {
        let mut app = test_app();
        app.note_shelf_added(item(
            "0000000000000001",
            "old.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));
        app.note_shelf_added(item(
            "0000000000000002",
            "new.png",
            "ada",
            "2026-01-01T11:00:00Z",
        ));

        let names: Vec<&str> = app
            .hub
            .shelf
            .items
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, vec!["new.png", "old.png"]);
        let toast = app.hub.shelf.toast.clone().expect("arrival toast");
        assert_eq!(toast.text, "📎 new.png — ada");
    }

    #[test]
    fn shelf_added_ignores_a_duplicate_file_id() {
        let mut app = test_app();
        app.note_shelf_added(item(
            "0000000000000001",
            "a.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));
        app.note_shelf_added(item(
            "0000000000000001",
            "a.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));

        assert_eq!(
            app.hub.shelf.items.len(),
            1,
            "the duplicate must be ignored"
        );
    }

    #[test]
    fn shelf_removed_drops_the_item_and_clamps_the_selection() {
        let mut app = test_app();
        app.note_shelf_added(item(
            "0000000000000001",
            "a.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));
        app.note_shelf_added(item(
            "0000000000000002",
            "b.png",
            "host",
            "2026-01-01T11:00:00Z",
        ));
        app.hub.shelf.sel = 1;

        app.note_shelf_removed("0000000000000002");

        assert_eq!(app.hub.shelf.items.len(), 1);
        assert_eq!(app.hub.shelf.sel, 0, "the selection must stay inside");
    }

    #[test]
    fn move_shelf_selection_wraps_and_stays_put_when_empty() {
        let mut app = test_app();
        app.move_shelf_selection(true);
        assert_eq!(app.hub.shelf.sel, 0, "no items: no movement");

        app.note_shelf_added(item(
            "0000000000000001",
            "a.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));
        app.note_shelf_added(item(
            "0000000000000002",
            "b.png",
            "host",
            "2026-01-01T11:00:00Z",
        ));
        app.move_shelf_selection(false);
        assert_eq!(app.hub.shelf.sel, 1, "up from the top wraps to the last");
        app.move_shelf_selection(true);
        assert_eq!(
            app.hub.shelf.sel, 0,
            "down from the bottom wraps to the top"
        );
    }

    #[test]
    fn delete_confirmation_removes_the_selected_item_without_a_client() {
        let mut app = test_app();
        app.note_shelf_added(item(
            "0000000000000001",
            "a.png",
            "host",
            "2026-01-01T10:00:00Z",
        ));
        app.hub.shelf.delete_confirm = true;

        app.delete_shelf_selection();

        assert!(app.hub.shelf.items.is_empty());
        assert!(!app.hub.shelf.delete_confirm, "the confirm flag clears");
    }

    /// The TTL check the refresh loop runs: a fresh toast stays, an old one
    /// goes.
    #[test]
    fn dismiss_shelf_toast_only_after_the_ttl() {
        let mut app = test_app();
        app.shelf_toast("📎 a.png — host".to_string());
        app.dismiss_shelf_toast();
        assert!(app.hub.shelf.toast.is_some(), "a fresh toast must stay");

        app.hub.shelf.toast.as_mut().unwrap().at -= TOAST_TTL + Duration::from_secs(1);
        app.dismiss_shelf_toast();
        assert!(app.hub.shelf.toast.is_none(), "an expired toast must go");
    }
}
