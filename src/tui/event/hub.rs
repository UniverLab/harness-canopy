//! CT31: key routing while the F1 Canopy hub is open.
//!
//! `handle_hub_key` is called first from `event::handle_key`; it returns
//! `true` for every key while the hub is open, so the hub is modal. The
//! Missions tab keeps the same ↑↓/jk selection the old legend had; the Keys
//! tab scrolls; the Settings tab has its editors.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::app::hub::HubTab;
use crate::tui::app::hub_settings::{dir_picker_at, SettingsEditor, SettingsFocus};
use crate::tui::app::types::App;

/// Route one key to the open hub. Returns `false` when the hub is closed, so
/// the caller falls through to the normal handler chain.
pub(super) fn handle_hub_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !app.hub_open {
        return false;
    }
    // F1 closes the hub from any state, even mid-edit or mid-filter.
    if code == KeyCode::F(1) {
        app.cancel_editor();
        app.hub.settings.filter_mode = false;
        app.close_hub();
        return true;
    }
    // The dir picker and the open editor take every key first.
    if app.hub.settings.dir_picker.is_some() {
        hub_dir_picker_key(app, code);
        return true;
    }
    if app.hub.settings.editor.is_some() {
        hub_editor_key(app, code, modifiers);
        return true;
    }
    if app.hub.settings.filter_mode {
        hub_filter_key(app, code);
        return true;
    }
    // An armed shelf delete owns every key: Esc cancels it instead of
    // closing the hub, and no tab switch escapes mid-confirm.
    if app.hub.tab == HubTab::Shelf && app.hub.shelf.delete_confirm {
        hub_shelf_key(app, code);
        return true;
    }
    // An armed remote revoke owns every key the same way.
    if app.hub.tab == HubTab::Remote && app.hub.remote.revoke_confirm.is_some() {
        hub_remote_confirm_key(app, code);
        return true;
    }
    // The open pairing overlay owns every key but Esc (which hides it):
    // no tab switch, digit jump or cursor move escapes it.
    if app.hub.tab == HubTab::Remote && app.hub.remote.pair_open && code != KeyCode::Esc {
        return true;
    }

    match code {
        KeyCode::Esc => hub_escape(app),
        KeyCode::Tab => app.cycle_hub_tab(true),
        KeyCode::BackTab => app.cycle_hub_tab(false),
        KeyCode::Left if modifiers.contains(KeyModifiers::SHIFT) => app.cycle_hub_tab(false),
        KeyCode::Right if modifiers.contains(KeyModifiers::SHIFT) => app.cycle_hub_tab(true),
        KeyCode::Char(c) if HubTab::from_digit(c).is_some() => {
            if let Some(tab) = HubTab::from_digit(c) {
                app.set_hub_tab(tab);
            }
        }
        _ if app.hub.tab == HubTab::Settings => hub_settings_key(app, code),
        _ if app.hub.tab == HubTab::Keys => hub_keys_key(app, code),
        _ if app.hub.tab == HubTab::Shelf => hub_shelf_key(app, code),
        _ if app.hub.tab == HubTab::Remote => app.hub_remote_key(code),
        _ => hub_missions_key(app, code),
    }
    true
}

/// Esc at the Settings properties level steps back to the categories; on
/// the Remote tab with the pairing overlay open it hides the overlay (the
/// code stays valid); everywhere else it closes the hub.
fn hub_escape(app: &mut App) {
    let at_properties =
        app.hub.tab == HubTab::Settings && app.hub.settings.focus == SettingsFocus::Properties;
    if at_properties {
        app.settings_back_to_categories();
    } else if app.hub.tab == HubTab::Remote && app.hub.remote.pair_open {
        app.close_pair_overlay();
    } else {
        app.close_hub();
    }
}

/// While a remote revoke is armed only `y`/Enter confirm and `n`/`Esc`
/// cancel — every other key, tab switches included, is swallowed.
fn hub_remote_confirm_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_remote_revoke(),
        KeyCode::Char('n') | KeyCode::Esc => app.cancel_remote_revoke(),
        _ => {}
    }
}

/// Settings keys, dispatched on the focus level (requirement 3).
fn hub_settings_key(app: &mut App, code: KeyCode) {
    match app.hub.settings.focus {
        SettingsFocus::Categories => hub_settings_categories_key(app, code),
        SettingsFocus::Properties => hub_settings_properties_key(app, code),
    }
}

/// Categories level: ↑↓ move the category, →/Enter enter its properties, `/`
/// filters. ←/→ never change a value.
fn hub_settings_categories_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Up => app.settings_move_group(false),
        KeyCode::Down => app.settings_move_group(true),
        KeyCode::Right | KeyCode::Enter => app.settings_enter_properties(),
        KeyCode::Char('/') => app.hub.settings.filter_mode = true,
        _ => {}
    }
}

/// Properties level: ↑↓ move the property, ← back to the categories, Enter/Space
/// activate the row, `r` resets it, `/` filters.
fn hub_settings_properties_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Up => app.settings_move_row(false),
        KeyCode::Down => app.settings_move_row(true),
        KeyCode::Left => app.settings_back_to_categories(),
        KeyCode::Enter | KeyCode::Char(' ') => app.settings_activate_property(),
        KeyCode::Char('r') => app.reset_selected_setting(),
        KeyCode::Char('/') => app.hub.settings.filter_mode = true,
        _ => {}
    }
}

/// The editor owns the keys: Esc cancels, Enter saves; PathList gets
/// `a add`, `d delete`, Shift+↑↓ reorder, ↑↓ to move the highlight.
fn hub_editor_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            app.cancel_editor();
            return;
        }
        KeyCode::Enter => {
            app.commit_editor_value();
            return;
        }
        _ => {}
    }

    let is_path_list = matches!(
        app.hub.settings.editor,
        Some(SettingsEditor::PathList { .. })
    );
    if is_path_list {
        match code {
            KeyCode::Char('a') => app.path_list_add(),
            KeyCode::Char('d') => app.path_list_delete(),
            KeyCode::Up if modifiers.contains(KeyModifiers::SHIFT) => app.path_list_move(false),
            KeyCode::Down if modifiers.contains(KeyModifiers::SHIFT) => app.path_list_move(true),
            KeyCode::Up => app.path_list_cursor(false),
            KeyCode::Down => app.path_list_cursor(true),
            _ => {}
        }
        return;
    }

    if let Some(SettingsEditor::Text { buffer, .. }) = app.hub.settings.editor.as_mut() {
        match code {
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Char(c) => buffer.push(c),
            _ => {}
        }
    }
}

/// `/` filter mode: Esc clears and leaves, Enter leaves keeping the text.
fn hub_filter_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.hub.settings.filter.clear();
            app.hub.settings.filter_mode = false;
            app.clamp_settings_selection();
        }
        KeyCode::Enter => app.hub.settings.filter_mode = false,
        KeyCode::Backspace => {
            app.hub.settings.filter.pop();
            app.clamp_settings_selection();
        }
        KeyCode::Char(c) => {
            app.hub.settings.filter.push(c);
            app.clamp_settings_selection();
        }
        KeyCode::Up => app.settings_move_row(false),
        KeyCode::Down => app.settings_move_row(true),
        _ => {}
    }
}

/// The Missions tab keeps the legend's ↑↓/jk selection across all missions.
fn hub_missions_key(app: &mut App, code: KeyCode) {
    let forward = match code {
        KeyCode::Down | KeyCode::Char('j') => true,
        KeyCode::Up | KeyCode::Char('k') => false,
        _ => return,
    };
    let mission_count = crate::domain::gamification::MISSIONS.len();
    if mission_count == 0 {
        return;
    }
    app.legend_selected =
        crate::tui::selection::move_index(app.legend_selected, mission_count, forward);
}

/// The Keys tab scrolls with ↑↓/jk.
fn hub_keys_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Up | KeyCode::Char('k') => app.move_keys_scroll(false),
        KeyCode::Down | KeyCode::Char('j') => app.move_keys_scroll(true),
        _ => {}
    }
}

/// The Shelf tab: ↑↓/jk move, Enter/Space open, `d` deletes behind a `y`/`n`
/// confirmation, `c` copies the path. While the confirmation is armed it
/// owns every key — `handle_hub_key` routes them here before the tab/digit/
/// `Esc` arms, so only `y`/Enter confirm and `n`/`Esc` cancel.
fn hub_shelf_key(app: &mut App, code: KeyCode) {
    if app.hub.shelf.delete_confirm {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => app.delete_shelf_selection(),
            KeyCode::Char('n') | KeyCode::Esc => app.hub.shelf.delete_confirm = false,
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Down | KeyCode::Char('j') => app.move_shelf_selection(true),
        KeyCode::Up | KeyCode::Char('k') => app.move_shelf_selection(false),
        KeyCode::Enter | KeyCode::Char(' ') => app.open_shelf_selection(),
        KeyCode::Char('d') => {
            if !app.hub.shelf.items.is_empty() {
                app.hub.shelf.delete_confirm = true;
            }
        }
        KeyCode::Char('c') => app.copy_shelf_selection_path(),
        _ => {}
    }
}

/// The PathList add picker: Esc cancels; ↑↓ move; Enter descends into the
/// highlighted directory; ←/Backspace go up; Space **selects the current
/// directory** into the PathList being edited (the picker closes).
fn hub_dir_picker_key(app: &mut App, code: KeyCode) {
    let Some(mut picker) = app.hub.settings.dir_picker.take() else {
        return;
    };
    match code {
        KeyCode::Esc => return,
        KeyCode::Up => picker.idx = picker.idx.saturating_sub(1),
        KeyCode::Down => {
            if picker.idx + 1 < picker.entries.len() {
                picker.idx += 1;
            }
        }
        KeyCode::Enter => {
            if let Some(name) = picker.entries.get(picker.idx).cloned() {
                picker = dir_picker_at(picker.current.join(name));
            }
        }
        KeyCode::Left | KeyCode::Backspace => {
            if let Some(parent) = picker.current.parent() {
                picker = dir_picker_at(parent.to_path_buf());
            }
        }
        KeyCode::Char(' ') => {
            let selected = picker.current.to_string_lossy().to_string();
            if let Some(SettingsEditor::PathList { list, idx, .. }) =
                app.hub.settings.editor.as_mut()
            {
                crate::tui::app::hub_settings::list_insert(list, idx, selected);
            }
            return;
        }
        _ => {}
    }
    app.hub.settings.dir_picker = Some(picker);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::StateRepository;
    use crate::db::Database;
    use crate::tui::app::hub_settings::SettingRow;
    use crate::tui::app::types::Focus;
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

    #[test]
    fn f1_opens_the_hub_and_esc_closes_it() {
        let mut app = test_app();
        app.focus = Focus::Home;
        assert!(!app.hub_open);

        crate::tui::event::handle_key(&mut app, KeyCode::F(1), KeyModifiers::NONE).unwrap();
        assert!(app.hub_open, "F1 from Home opens the hub");

        crate::tui::event::handle_key(&mut app, KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(!app.hub_open, "Esc closes the hub");
    }

    #[test]
    fn tab_and_digits_switch_tabs() {
        let mut app = test_app();
        app.open_hub();
        assert_eq!(app.hub.tab, HubTab::Settings);

        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Keys);
        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Missions);
        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Shelf);
        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote);
        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Settings, "Tab wraps forward");

        handle_hub_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Keys);

        handle_hub_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote, "digit 5 opens Remote");
        handle_hub_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Keys);

        handle_hub_key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Settings, "BackTab wraps backwards");
    }

    #[test]
    fn hub_swallows_other_keys() {
        let mut app = test_app();
        app.focus = Focus::Agent;
        app.open_hub();
        assert!(app.running);

        let handled = handle_hub_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);

        assert!(handled, "the hub consumes the key");
        assert!(app.running, "q must not reach the agent-focus quit arm");
    }

    /// The Shelf tab's keys: ↑↓/jk move over the items, `d` arms the
    /// confirmation, `n` cancels it and `y` deletes the selected item.
    #[test]
    fn shelf_tab_moves_and_deletes_behind_a_confirmation() {
        use crate::sessions::protocol::ShelfItem;

        let mut app = test_app();
        let item = |file_id: &str, name: &str| ShelfItem {
            file_id: file_id.to_string(),
            session_id: None,
            name: name.to_string(),
            mime: "image/png".to_string(),
            size: 1024,
            caption: None,
            from_label: "host".to_string(),
            created_at: "2026-01-01T10:00:00Z".to_string(),
        };
        app.open_hub();
        app.set_hub_tab(HubTab::Shelf);
        app.hub.shelf.items = vec![
            item("0000000000000001", "a.png"),
            item("0000000000000002", "b.png"),
        ];

        handle_hub_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.hub.shelf.sel, 1, "j moves down the shelf");
        handle_hub_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.hub.shelf.sel, 0, "k moves back up");

        handle_hub_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(app.hub.shelf.delete_confirm, "d arms the confirmation");
        // Any other key mid-confirm is swallowed and changes nothing — the
        // tab shortcuts included, so the hub cannot switch tabs under it.
        handle_hub_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.hub.shelf.sel, 0, "movement is off while confirming");
        assert!(app.hub.shelf.delete_confirm);
        handle_hub_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Shelf, "digits do not switch tabs");
        assert!(app.hub.shelf.delete_confirm);
        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            !app.hub.shelf.delete_confirm,
            "Esc cancels the confirmation"
        );
        assert!(app.hub_open, "Esc must not close the hub mid-confirm");
        assert_eq!(app.hub.shelf.items.len(), 2, "Esc deletes nothing");

        handle_hub_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        handle_hub_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(!app.hub.shelf.delete_confirm, "n cancels the confirmation");
        assert_eq!(app.hub.shelf.items.len(), 2, "n deletes nothing");

        handle_hub_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        handle_hub_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(!app.hub.shelf.delete_confirm, "y confirms and clears");
        assert_eq!(app.hub.shelf.items.len(), 1, "y deletes the selected item");
        assert_eq!(app.hub.shelf.items[0].name, "b.png", "the top item went");

        // Without a pending confirmation Esc closes the hub as everywhere.
        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.hub_open, "Esc closes the hub at the top level");
    }

    #[test]
    fn missions_tab_keeps_the_selection_keys() {
        use crate::domain::gamification::MISSIONS;
        use crate::tui::gamification::{MissionEvent, MissionSnapshot};

        let mut app = test_app();
        // Unlock three missions through the real path so the cursor has
        // visited entries; the tab itself lists all 28 missions.
        let events = [
            MissionEvent::FireflyCaught,
            MissionEvent::WorldLinked,
            MissionEvent::IdentityEvolved,
        ];
        app.mission_manager
            .process_refresh(&MissionSnapshot::default(), &events, &mut app.whimsg)
            .unwrap();
        assert!(app.mission_manager.unlocked_count() >= 3);

        app.open_hub();
        app.set_hub_tab(HubTab::Missions);
        app.legend_selected = 0;

        handle_hub_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.legend_selected, 1);
        handle_hub_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.legend_selected, 0);
        // Up from zero wraps to the last mission of the full list.
        handle_hub_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.legend_selected, MISSIONS.len() - 1);
    }

    fn enum_row() -> SettingRow {
        SettingRow {
            key: "theme".to_string(),
            group: "General".to_string(),
            description: "theme description".to_string(),
            effective: "\"classic\"".to_string(),
            default: "\"classic\"".to_string(),
            source: "file".to_string(),
            status: "ok".to_string(),
        }
    }

    fn bool_row() -> SettingRow {
        SettingRow {
            key: "tui.atmosphere".to_string(),
            group: "General".to_string(),
            description: "atmosphere description".to_string(),
            effective: "true".to_string(),
            default: "true".to_string(),
            source: "file".to_string(),
            status: "ok".to_string(),
        }
    }

    /// From a freshly opened hub, → only ever enters the properties level: it
    /// must never change a setting value (and never spawn a write).
    #[test]
    fn pressing_right_five_times_never_changes_a_setting_value() {
        let mut app = test_app();
        app.open_hub();
        app.hub.settings.rows = vec![enum_row(), bool_row()];
        let before: Vec<String> = app
            .hub
            .settings
            .rows
            .iter()
            .map(|row| row.effective.clone())
            .collect();

        for _ in 0..5 {
            handle_hub_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        }

        assert!(
            app.hub.settings.write_rx.is_none(),
            "→ must never spawn a settings write"
        );
        let after: Vec<String> = app
            .hub
            .settings
            .rows
            .iter()
            .map(|row| row.effective.clone())
            .collect();
        assert_eq!(before, after, "→ must not change any setting value");
        assert_eq!(app.hub.settings.focus, SettingsFocus::Properties);
        assert!(app.hub_open, "→ keeps the hub open");
    }

    #[test]
    fn shift_arrows_switch_tabs() {
        let mut app = test_app();
        app.open_hub();
        assert_eq!(app.hub.tab, HubTab::Settings);

        handle_hub_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!(app.hub.tab, HubTab::Keys);
        handle_hub_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
        assert_eq!(app.hub.tab, HubTab::Missions);
        handle_hub_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(app.hub.tab, HubTab::Keys);
        handle_hub_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
        assert_eq!(app.hub.tab, HubTab::Settings, "Shift+← wraps backwards");
    }

    #[test]
    fn right_enters_properties_and_left_returns_keeping_the_category() {
        let mut app = test_app();
        app.open_hub();
        app.hub.settings.group_idx = 2;
        assert_eq!(app.hub.settings.focus, SettingsFocus::Categories);

        handle_hub_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.hub.settings.focus, SettingsFocus::Properties);
        assert_eq!(app.hub.settings.row_idx, 0, "first property selected");

        handle_hub_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.hub.settings.focus, SettingsFocus::Categories);
        assert_eq!(app.hub.settings.group_idx, 2, "the category is kept");
    }

    #[test]
    fn esc_returns_from_properties_and_closes_from_categories() {
        let mut app = test_app();
        app.open_hub();

        handle_hub_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.hub.settings.focus, SettingsFocus::Properties);

        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.hub_open, "Esc at properties only steps back");
        assert_eq!(app.hub.settings.focus, SettingsFocus::Categories);

        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.hub_open, "Esc at the categories level closes the hub");
    }

    /// Spawn a fake daemon whose every tool call answers `{}` successfully.
    fn ok_result() -> serde_json::Value {
        serde_json::json!({ "content": [{ "type": "text", "text": "{}" }], "isError": false })
    }

    /// Enter on an enum row (at the properties level) cycles it forward.
    #[test]
    fn enter_on_an_enum_row_cycles_it_forward() {
        let fake = crate::tui::mcp_client::test_support::spawn_fake_daemon(ok_result());
        let mut app = test_app();
        app.db.set_state("port", &fake.port).unwrap();
        app.open_hub();
        app.hub.settings.rows = vec![enum_row()];
        app.hub.settings.focus = SettingsFocus::Properties;

        handle_hub_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        for _ in 0..1000 {
            if app.hub.settings.write_rx.is_none() {
                break;
            }
            app.poll_settings_write();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["name"], "config");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "set", "key": "theme", "value": "modern" })
        );
    }

    fn remote_device(id: &str) -> crate::remote::panel::DeviceView {
        crate::remote::panel::DeviceView {
            id: id.to_string(),
            label: "laptop".to_string(),
            last_seen: None,
            live: false,
        }
    }

    /// Esc with the pairing overlay open hides the overlay — the hub stays
    /// open and the code stays; a second Esc closes the hub.
    #[test]
    fn esc_hides_the_pair_overlay_before_closing_the_hub() {
        let mut app = test_app();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.pair = Some(crate::remote::panel::PairView {
            code: "7K3QH-M9P2X".to_string(),
            link: "https://remote.univerlab.org/pair#7K3QH-M9P2X".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 600,
            qr: "qr".to_string(),
        });
        app.hub.remote.pair_open = true;

        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.hub.remote.pair_open, "Esc hides the overlay");
        assert!(app.hub_open, "the hub stays open");
        assert!(app.hub.remote.pair.is_some(), "the code stays valid");

        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.hub_open, "the second Esc closes the hub");
    }

    /// The open pairing overlay owns every key but Esc: tab switches,
    /// digits, movement, `r` and Enter all bounce off it.
    #[test]
    fn the_pair_overlay_swallows_every_key_but_esc() {
        let mut app = test_app();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.pair = Some(crate::remote::panel::PairView {
            code: "7K3QH-M9P2X".to_string(),
            link: "https://remote.univerlab.org/pair#7K3QH-M9P2X".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 600,
            qr: "qr".to_string(),
        });
        app.hub.remote.pair_open = true;
        app.hub.remote.view.devices = vec![remote_device("bfcba2b742a807bb")];

        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote, "Tab must not switch tabs");
        handle_hub_key(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote, "BackTab must not either");
        handle_hub_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote, "digits must not jump tabs");
        handle_hub_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 0, "the cursor cannot move");
        handle_hub_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.revoke_confirm, None, "no arming under it");
        handle_hub_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            matches!(
                app.hub.remote.login,
                crate::tui::app::hub_remote::LoginFlow::Idle
            ),
            "Enter must not start the device flow under the overlay"
        );

        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.hub.remote.pair_open, "Esc still hides the overlay");
        assert!(app.hub_open, "the hub stays open");
        handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Settings, "keys work again after");
    }

    /// `r` on a device row arms the revoke, `n`/Esc cancel it, `y` revokes
    /// through the daemon — and nothing else acts mid-confirm.
    #[test]
    fn remote_revoke_runs_behind_a_y_n_confirmation() {
        let fake = crate::tui::mcp_client::test_support::spawn_fake_daemon(ok_result());
        let mut app = test_app();
        app.db.set_state("port", &fake.port).unwrap();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.view.devices = vec![remote_device("bfcba2b742a807bb")];
        app.hub.remote.row_idx = 4; // Device(0).

        handle_hub_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        assert_eq!(
            app.hub.remote.revoke_confirm.as_deref(),
            Some("bfcba2b742a807bb"),
            "r arms the confirmation"
        );
        handle_hub_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
        assert_eq!(app.hub.tab, HubTab::Remote, "digits do not switch tabs");
        assert!(app.hub.remote.revoke_confirm.is_some());
        handle_hub_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.hub.remote.revoke_confirm, None, "Esc cancels");
        assert!(app.hub_open, "Esc must not close the hub mid-confirm");

        handle_hub_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        handle_hub_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.revoke_confirm, None, "n cancels");

        handle_hub_key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        handle_hub_key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.revoke_confirm, None, "y confirms and clears");
        for _ in 0..1000 {
            if app.hub.remote.write_rx.is_none() {
                break;
            }
            app.poll_remote_write();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["name"], "remote");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "revoke", "device_id": "bfcba2b742a807bb" })
        );
    }

    /// `d` on a device row disconnects through the daemon at once — no
    /// confirm — and off a device row it sends nothing.
    #[test]
    fn remote_disconnect_sends_the_disconnect_action_at_once() {
        let fake = crate::tui::mcp_client::test_support::spawn_fake_daemon(ok_result());
        let mut app = test_app();
        app.db.set_state("port", &fake.port).unwrap();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.view.devices = vec![remote_device("bfcba2b742a807bb")];

        app.hub.remote.row_idx = 0; // GitHub.
        handle_hub_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(
            fake.recorded_calls().is_empty(),
            "d off a device row sends nothing"
        );

        app.hub.remote.row_idx = 4; // Device(0).
        handle_hub_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.revoke_confirm, None, "no confirm is armed");
        for _ in 0..1000 {
            if app.hub.remote.write_rx.is_none() {
                break;
            }
            app.poll_remote_write();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0]["name"], "remote");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "action": "disconnect", "device_id": "bfcba2b742a807bb" })
        );
    }

    /// The Remote tab moves with ↑↓/jk and stays put on other keys.
    #[test]
    fn remote_tab_moves_the_cursor_with_arrows_and_jk() {
        let mut app = test_app();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        assert_eq!(app.hub.remote.row_idx, 0);

        handle_hub_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 1);
        handle_hub_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 2);
        handle_hub_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 1);
        handle_hub_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 0);
        // Up from zero wraps to the last row (Pair, with no devices).
        handle_hub_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.hub.remote.row_idx, 3);
    }
}
