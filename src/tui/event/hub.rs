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
        _ => hub_missions_key(app, code),
    }
    true
}

/// Esc at the Settings properties level steps back to the categories;
/// everywhere else — including the categories level — it closes the hub.
fn hub_escape(app: &mut App) {
    let at_properties =
        app.hub.tab == HubTab::Settings && app.hub.settings.focus == SettingsFocus::Properties;
    if at_properties {
        app.settings_back_to_categories();
    } else {
        app.close_hub();
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
        assert_eq!(app.hub.tab, HubTab::Settings, "Tab wraps forward");

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
        assert_eq!(calls[0]["name"], "config_set");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "key": "theme", "value": "modern" })
        );
    }
}
