//! Hint selection, F1 context rule, tip actions, Help tab and marker
//! rendering. All persistence uses temp dirs, never `~/.canopy`.

use std::sync::Arc;

use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Terminal;

use crate::db::Database;
use crate::tui::app::hub::HubTab;
use crate::tui::app::types::{AgentEntry, App, Focus};
use crate::tui::hints::{active_hint, HINTS};

fn test_app() -> (App, tempfile::TempDir) {
    let path = crate::test_support::temp_db_path();
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    let app = App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    (app, data_dir)
}

/// A wide, tall terminal: hints are never suppressed for size.
fn wide(app: &mut App) {
    app.term_width = 120;
    app.term_height = 30;
}

fn press(app: &mut App, code: KeyCode) {
    crate::tui::event::handle_key(app, code, KeyModifiers::empty()).unwrap();
}

fn click(app: &mut App, column: u16, row: u16) -> bool {
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::empty(),
    };
    crate::tui::hints::handle_hint_mouse(app, &mouse)
}

fn draw_text(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| crate::tui::ui::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

#[test]
fn empty_home_shows_start_session() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    assert!(matches!(app.focus, Focus::Home));
    let hint = active_hint(&app).expect("a hint is active on an empty start screen");
    assert_eq!(hint.id, "start_session");
}

#[test]
fn open_session_shows_prompt_menu() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.agents.push(AgentEntry::Interactive(0));
    app.focus = Focus::Agent;
    let hint = active_hint(&app).expect("a hint is active with a session open");
    assert_eq!(hint.id, "prompt_menu");
}

#[test]
fn home_with_sessions_shows_no_hint() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.agents.push(AgentEntry::Interactive(0));
    assert!(
        matches!(app.focus, Focus::Home),
        "a new session starts on Home until focused"
    );
    assert!(
        active_hint(&app).is_none(),
        "the prompt-strip anchor is off screen on Home"
    );
}

#[test]
fn open_hub_shows_hub_help() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    let hint = active_hint(&app).expect("a hint is active with the hub open");
    assert_eq!(hint.id, "hub_help");
}

#[test]
fn dismissed_hint_never_shows_again() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let dir = app.data_dir.clone();
    app.cli_usage.dismiss_hint(&dir, "start_session");
    assert!(active_hint(&app).is_none(), "dismissed hint stays gone");
}

#[test]
fn hidden_hints_show_nothing() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let dir = app.data_dir.clone();
    app.cli_usage.hide_all_hints(&dir);
    assert!(active_hint(&app).is_none());
    app.hub_open = true;
    assert!(
        active_hint(&app).is_none(),
        "hidden covers the hub hint too"
    );
}

#[test]
fn narrow_terminal_hides_hints() {
    let (mut app, _dir) = test_app();
    app.term_width = 79;
    app.term_height = 30;
    assert!(active_hint(&app).is_none(), "width < 80 hides hints");
    app.term_width = 120;
    app.term_height = 19;
    assert!(active_hint(&app).is_none(), "height < 20 hides hints");
}

#[test]
fn f1_opens_tip_when_hint_active() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    press(&mut app, KeyCode::F(1));
    assert_eq!(app.hint_tip_open, Some("start_session"));
    assert!(!app.hub_open, "the hub stays closed when a hint is showing");
}

#[test]
fn f1_closes_the_open_tip() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    press(&mut app, KeyCode::F(1));
    assert_eq!(app.hint_tip_open, None);
    assert!(!app.hub_open);
}

#[test]
fn f1_opens_hub_when_no_hint_active() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let dir = app.data_dir.clone();
    app.cli_usage.hide_all_hints(&dir);
    press(&mut app, KeyCode::F(1));
    assert!(app.hub_open);
    assert_eq!(app.hint_tip_open, None);
}

#[test]
fn f1_closes_hub_and_tip_together() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    app.hint_tip_open = Some("hub_help");
    press(&mut app, KeyCode::F(1));
    assert!(!app.hub_open, "F1 always closes the hub");
    assert_eq!(app.hint_tip_open, None, "the tip closes with its anchor");
}

#[test]
fn hub_open_tip_y_dismisses_and_keeps_hub() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    app.hint_tip_open = Some("hub_help");
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.hint_tip_open, None, "Got it closes the tip");
    assert!(app.hub_open, "the hub stays open");
    assert!(app
        .cli_usage
        .hints_dismissed
        .contains(&"hub_help".to_string()));
}

#[test]
fn hub_open_tip_esc_closes_tip_and_keeps_hub() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    app.hint_tip_open = Some("hub_help");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.hint_tip_open, None, "Esc closes the tip");
    assert!(app.hub_open, "the hub stays open");
    assert!(app.cli_usage.hints_dismissed.is_empty());
    assert!(!app.cli_usage.hints_hidden);
}

#[test]
fn hub_tab_switch_drops_the_tip() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    app.hint_tip_open = Some("hub_help");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.hint_tip_open, None, "the anchor left the screen");
    assert!(app.hub_open);
}

#[test]
fn f1_from_agent_focus_opens_tip_not_hub() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.agents.push(AgentEntry::Interactive(0));
    app.focus = Focus::Agent;
    press(&mut app, KeyCode::F(1));
    assert_eq!(app.hint_tip_open, Some("prompt_menu"));
    assert!(!app.hub_open);
}

#[test]
fn tip_y_dismisses_and_persists() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.hint_tip_open, None);
    assert!(app
        .cli_usage
        .hints_dismissed
        .contains(&"start_session".to_string()));
    assert!(
        active_hint(&app).is_none(),
        "dismissed hint never shows again"
    );
}

#[test]
fn tip_enter_dismisses_like_y() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.hint_tip_open, None);
    assert!(app
        .cli_usage
        .hints_dismissed
        .contains(&"start_session".to_string()));
}

#[test]
fn tip_n_hides_all_hints() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.agents.push(AgentEntry::Interactive(0));
    app.hint_tip_open = Some("prompt_menu");
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(app.hint_tip_open, None);
    assert!(app.cli_usage.hints_hidden);
    assert!(active_hint(&app).is_none());
}

#[test]
fn tip_esc_closes_without_persisting() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.hint_tip_open, None);
    assert!(app.cli_usage.hints_dismissed.is_empty());
    assert!(!app.cli_usage.hints_hidden);
}

#[test]
fn other_keys_fall_through_while_tip_open() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    press(&mut app, KeyCode::Char('z'));
    assert_eq!(
        app.hint_tip_open,
        Some("start_session"),
        "unrelated keys keep the tip"
    );
}

#[test]
fn marker_click_opens_tip_and_action_click_dismisses() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    draw_text(&mut app, 120, 30);
    let marker = app
        .hint_marker
        .expect("marker drawn on the empty home screen");
    assert!(
        click(&mut app, marker.x, marker.y),
        "marker click opens the tip"
    );
    assert_eq!(app.hint_tip_open, Some("start_session"));
    draw_text(&mut app, 120, 30);
    let got_it = app.hint_tip_got_it.expect("tip action drawn");
    assert!(
        click(&mut app, got_it.x, got_it.y),
        "Got it click dismisses"
    );
    assert_eq!(app.hint_tip_open, None);
    assert!(app
        .cli_usage
        .hints_dismissed
        .contains(&"start_session".to_string()));
}

#[test]
fn hub_header_marker_opens_tip_on_click() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hub_open = true;
    draw_text(&mut app, 120, 30);
    let marker = app.hint_marker.expect("marker drawn in the hub header");
    click(&mut app, marker.x, marker.y);
    assert_eq!(app.hint_tip_open, Some("hub_help"));
}

#[test]
fn marker_draws_as_bracketed_question_mark() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let text = draw_text(&mut app, 120, 30);
    assert!(text.contains("[?]"), "the pulsing marker renders: {text}");
    assert!(
        !text.contains("Ctrl+N"),
        "the tip stays closed until clicked"
    );
}

#[test]
fn open_tip_renders_text_key_and_actions() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    app.hint_tip_open = Some("start_session");
    let text = draw_text(&mut app, 120, 30);
    assert!(text.contains("Ctrl+N"), "tip names the key: {text}");
    assert!(text.contains("Got it"), "tip offers Got it: {text}");
    assert!(text.contains("Hide all"), "tip offers Hide all: {text}");
}

#[test]
fn help_tab_shows_keys_graph_line_and_reset() {
    let (mut app, _dir) = test_app();
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    let text = draw_text(&mut app, 120, 30);
    for key in ["Ctrl+N", "F1", "F5", "Enter", "Esc", "Ctrl+Q"] {
        assert!(text.contains(key), "Help tab names {key}: {text}");
    }
    assert!(
        text.contains("reusable workflow of agent, check, and gate nodes"),
        "graph one-liner: {text}"
    );
    assert!(text.contains("Reset hints"), "reset action: {text}");
}

#[test]
fn help_r_resets_hints() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let dir = app.data_dir.clone();
    app.cli_usage.dismiss_hint(&dir, "start_session");
    app.cli_usage.hide_all_hints(&dir);
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    press(&mut app, KeyCode::Char('r'));
    assert!(app.cli_usage.hints_dismissed.is_empty());
    assert!(!app.cli_usage.hints_hidden);
    assert!(active_hint(&app).is_some(), "hints are back after reset");
}

#[test]
fn help_reset_click_resets_hints() {
    let (mut app, _dir) = test_app();
    wide(&mut app);
    let dir = app.data_dir.clone();
    app.cli_usage.hide_all_hints(&dir);
    app.hub_open = true;
    app.hub.tab = HubTab::Help;
    draw_text(&mut app, 120, 30);
    let reset = app.hub.help_reset_rect.expect("reset row drawn");
    assert!(click(&mut app, reset.x, reset.y), "reset click resets");
    assert!(!app.cli_usage.hints_hidden);
}

#[test]
fn marker_pulse_is_static_when_animations_off() {
    use crate::domain::animations_mode::AnimationsMode;
    use crate::tui::ui::theme::Theme;
    let (mut app, _dir) = test_app();
    app.tui_settings.animations = AnimationsMode::Off;
    let theme = Theme::classic();
    app.animation_tick = 0;
    let calm = crate::tui::hints::tick_style(&app, &theme);
    app.animation_tick = 25;
    let later = crate::tui::hints::tick_style(&app, &theme);
    assert_eq!(calm, later, "animations off freezes the pulse");
}

#[test]
fn marker_pulse_alternates_when_animations_on() {
    use crate::domain::animations_mode::AnimationsMode;
    use crate::tui::ui::theme::Theme;
    let (mut app, _dir) = test_app();
    app.tui_settings.animations = AnimationsMode::Full;
    app.tui_settings.atmosphere = true;
    let theme = Theme::classic();
    app.animation_tick = 0;
    let bright = crate::tui::hints::tick_style(&app, &theme);
    app.animation_tick = 10;
    let dim = crate::tui::hints::tick_style(&app, &theme);
    assert_ne!(bright, dim, "the marker alternates about every 500 ms");
}

#[test]
fn hints_table_holds_the_three_spec_hints_in_order() {
    let ids: Vec<&str> = HINTS.iter().map(|hint| hint.id).collect();
    assert_eq!(ids, vec!["start_session", "prompt_menu", "hub_help"]);
}
