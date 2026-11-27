//! CT31: key routing while the F1 Canopy hub is open.
//!
//! `handle_hub_key` runs early from `event::handle_key` (after an open hint
//! tip's action keys, except F1); it returns `true` for every other key
//! while the hub is open, so the hub is modal. The
//! Missions tab keeps the same ↑↓/jk selection the old legend had; the Keys
//! tab scrolls; the Settings tab has its editors.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::app::hub::HubTab;
use crate::tui::app::hub_settings::{dir_picker_at, SettingsEditor, SettingsFocus};
use crate::tui::app::types::App;

/// Route one key to the open hub. Returns `false` when the hub is closed, so
/// the caller falls through to the normal handler chain. Runs after an open
/// hint tip's action keys (except F1, which always closes the hub first).
pub(super) fn handle_hub_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !app.hub_open {
        return false;
    }
    // F1 closes the hub from any state, even mid-edit or mid-filter —
    // and any open hint tip with it, whose anchor leaves the screen.
    if code == KeyCode::F(1) {
        app.cancel_editor();
        app.hub.settings.filter_mode = false;
        app.close_hub();
        app.hint_tip_open = None;
        return true;
    }
    // A tab switch moves the HubHeader anchor off screen; drop the tip so
    // it never points at a hint whose marker just disappeared.
    let tab_before = app.hub.tab;
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
    // An armed trust-remove owns every key the same way.
    if app.hub.tab == HubTab::Remote && app.hub.remote.trust_remove_confirm.is_some() {
        hub_trust_remove_confirm_key(app, code);
        return true;
    }
    // The open pairing overlay owns every key but Esc (which hides it):
    // no tab switch, digit jump or cursor move escapes it.
    if app.hub.tab == HubTab::Remote && app.hub.remote.pair_open && code != KeyCode::Esc {
        return true;
    }
    // The open trust form owns every key too, but it edits with them.
    if app.hub.tab == HubTab::Remote && app.hub.remote.trust.open {
        app.hub_remote_key(code);
        return true;
    }
    // The open calendar add form owns every key too, including Tab/digits/Esc.
    if app.hub.tab == HubTab::Calendar && app.hub.calendar.add_form.open {
        super::hub_calendar::handle_hub_calendar_key(app, code);
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
        _ if app.hub.tab == HubTab::Calendar => {
            super::hub_calendar::handle_hub_calendar_key(app, code);
        }
        _ if app.hub.tab == HubTab::Help => crate::tui::hints::handle_help_key(app, code),
        _ => hub_missions_key(app, code),
    }
    if app.hub.tab != tab_before {
        app.hint_tip_open = None;
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

/// While a trust-remove is armed only `y`/Enter confirm and `n`/`Esc`
/// cancel — every other key, tab switches included, is swallowed.
fn hub_trust_remove_confirm_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('y') | KeyCode::Enter => app.confirm_trust_remove(),
        KeyCode::Char('n') | KeyCode::Esc => app.cancel_trust_remove(),
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

pub(super) fn handle_hub_scroll(app: &mut App, dir: i32) {
    match app.hub.tab {
        HubTab::Missions => {
            let mission_count = crate::domain::gamification::MISSIONS.len();
            if mission_count != 0 {
                app.legend_selected =
                    crate::tui::selection::move_index(app.legend_selected, mission_count, dir < 0);
            }
        }
        HubTab::Shelf => app.move_shelf_selection(dir < 0),
        HubTab::Calendar => app.move_calendar_selection(dir < 0),
        _ => {}
    }
}

#[cfg(test)]
#[path = "hub_tests.rs"]
mod tests;
