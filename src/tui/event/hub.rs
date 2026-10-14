//! CT31: key routing while the F1 Canopy hub is open.
//!
//! `handle_hub_key` is called first from `event::handle_key`; it returns
//! `true` for every key while the hub is open, so the hub is modal. The
//! Missions tab keeps the same ↑↓/jk selection the old legend had; the Keys
//! tab scrolls; the Settings tab has its editors.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::domain::settings::{find, Kind};
use crate::tui::app::hub::HubTab;
use crate::tui::app::hub_settings::{dir_picker_at, SettingsEditor};
use crate::tui::app::types::App;

/// Route one key to the open hub. Returns `false` when the hub is closed, so
/// the caller falls through to the normal handler chain.
pub(super) fn handle_hub_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !app.hub_open {
        return false;
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
        KeyCode::F(1) | KeyCode::Esc => app.close_hub(),
        KeyCode::Tab => app.cycle_hub_tab(true),
        KeyCode::BackTab => app.cycle_hub_tab(false),
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

/// ↑↓ move rows; ←→ cycle an enum row, or step the group column otherwise;
/// Space toggles a bool; Enter opens the editor; `r` resets; `/` filters.
fn hub_settings_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Up => app.settings_move_row(false),
        KeyCode::Down => app.settings_move_row(true),
        KeyCode::Left | KeyCode::Right => {
            let forward = code == KeyCode::Right;
            let is_enum = app
                .selected_setting()
                .and_then(|row| find(&row.key))
                .is_some_and(|def| matches!(def.kind, Kind::Enum { .. }));
            if is_enum {
                app.cycle_enum(forward);
            } else {
                app.settings_move_group(forward);
            }
        }
        KeyCode::Char(' ') => app.toggle_bool(),
        KeyCode::Enter => app.open_setting_editor(),
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

/// The Missions tab keeps the legend's ↑↓/jk selection.
fn hub_missions_key(app: &mut App, code: KeyCode) {
    let forward = match code {
        KeyCode::Down | KeyCode::Char('j') => true,
        KeyCode::Up | KeyCode::Char('k') => false,
        _ => return,
    };
    let unlocked_count = app.mission_manager.unlocked_count();
    if unlocked_count == 0 {
        return;
    }
    app.legend_selected =
        crate::tui::selection::move_index(app.legend_selected, unlocked_count, forward);
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
    use crate::db::Database;
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
        use crate::tui::gamification::{MissionEvent, MissionSnapshot};

        let mut app = test_app();
        // Unlock three missions through the real path so the cursor has room
        // to move and wrap.
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
        // Up from zero wraps to the last unlocked mission.
        let count = app.mission_manager.unlocked_count();
        handle_hub_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
        assert_eq!(app.legend_selected, count - 1);
    }
}
