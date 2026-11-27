use super::*;
use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::tui::app::hub_settings::SettingRow;
use crate::tui::app::types::Focus;
use std::sync::Arc;

fn test_app() -> App {
    let path = crate::test_support::temp_db_path();
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
    assert_eq!(app.hub.tab, HubTab::Calendar, "Tab reaches Calendar");
    handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Help, "Tab reaches the Help tab");
    handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Settings, "Tab wraps forward");

    handle_hub_key(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Keys);

    handle_hub_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Remote, "digit 5 opens Remote");
    handle_hub_key(&mut app, KeyCode::Char('6'), KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Calendar, "digit 6 opens Calendar");
    handle_hub_key(&mut app, KeyCode::Char('7'), KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Help, "digit 7 opens Help");
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
    // visited entries; the tab itself lists all 24 missions.
    let events = [
        MissionEvent::FireflyCaught,
        MissionEvent::WorldLinked,
        MissionEvent::YoloTaskCompleted,
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
        paired_at: "2026-09-01T09:00:00Z".to_string(),
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
    assert_eq!(app.hub.tab, HubTab::Calendar, "keys work again after");
    handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Help, "Tab advances to Help");
    handle_hub_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.hub.tab, HubTab::Settings, "Tab wraps past Help");
}

/// `r` on a device row arms the revoke, `n`/Esc cancel it, `y` revokes
/// through the admin route — and nothing else acts mid-confirm.
#[test]
fn remote_revoke_runs_behind_a_y_n_confirmation() {
    let fake = crate::remote::admin_client::test_support::spawn_fake_admin(
        200,
        r#"{"revoked":"bfcba2b742a807bb"}"#,
    );
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
    let requests = fake.recorded();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].request_line, "POST /admin/remote HTTP/1.1");
    let sent: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("the admin body is JSON");
    assert_eq!(
        sent,
        serde_json::json!({
            "action": "revoke",
            "device_id": "bfcba2b742a807bb",
            "confirm": true,
        }),
        "the revoke carries confirm:true"
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
