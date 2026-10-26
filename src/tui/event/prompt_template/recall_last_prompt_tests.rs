use super::handle_prompt_template_key;
use crate::db::Database;
use crate::tui::app::types::App;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_app() -> (App, tempfile::TempDir) {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).expect("create test db"));
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    (app, data_dir)
}

fn press(app: &mut App, code: KeyCode) {
    handle_prompt_template_key(app, code, KeyModifiers::NONE).expect("key handled");
}

fn ctrl_l(app: &mut App) {
    handle_prompt_template_key(app, KeyCode::Char('l'), KeyModifiers::CONTROL)
        .expect("ctrl+l handled");
}

fn ctrl_e(app: &mut App) {
    handle_prompt_template_key(app, KeyCode::Char('e'), KeyModifiers::CONTROL)
        .expect("ctrl+e handled");
}

#[test]
fn ctrl_e_toggles_between_normal_and_raw_tabs() {
    use crate::tui::app::dialog::PromptTab;
    let (mut app, _dir) = test_app();
    app.open_simple_prompt_dialog(None);
    assert_eq!(
        app.simple_prompt_dialog.as_ref().unwrap().active_tab,
        PromptTab::Normal
    );

    ctrl_e(&mut app);
    assert_eq!(
        app.simple_prompt_dialog.as_ref().unwrap().active_tab,
        PromptTab::Raw
    );

    ctrl_e(&mut app);
    assert_eq!(
        app.simple_prompt_dialog.as_ref().unwrap().active_tab,
        PromptTab::Normal
    );
}

#[test]
fn typing_in_the_raw_tab_fills_the_raw_buffer_only() {
    use crate::tui::app::dialog::PromptTab;
    let (mut app, _dir) = test_app();
    app.open_simple_prompt_dialog(None);
    ctrl_e(&mut app); // → Raw

    for ch in "/compact".chars() {
        press(&mut app, KeyCode::Char(ch));
    }

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert_eq!(dialog.active_tab, PromptTab::Raw);
    assert_eq!(dialog.raw_text(), "/compact");
    // The Normal-form instruction section is untouched by raw typing.
    assert_eq!(dialog.get_section_content("instruction_1"), "");
}

#[test]
fn ctrl_l_with_nothing_stored_leaves_empty_builder_untouched() {
    let (mut app, _dir) = test_app();
    app.open_simple_prompt_dialog(None);

    ctrl_l(&mut app);

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.is_empty());
    assert!(dialog.pending_recall.is_none());
}

#[test]
fn fresh_open_focuses_the_first_section() {
    let (mut app, _dir) = test_app();
    app.open_simple_prompt_dialog(None);
    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert_eq!(dialog.focused_section, 1);
    assert!(dialog.focused_section_name().is_some());
}

#[test]
fn open_with_initial_content_focuses_first_section_not_send_control() {
    let (mut app, _dir) = test_app();
    let mut content = std::collections::HashMap::new();
    content.insert("instruction".to_string(), "prefilled task".to_string());
    app.open_simple_prompt_dialog(Some(content));

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    // Previously this path parked focus on the send control (index 0).
    assert_eq!(dialog.focused_section, 1);
    assert!(dialog.focused_section_name().is_some());
}

#[test]
fn ctrl_l_recalls_immediately_into_an_empty_builder() {
    let (mut app, _dir) = test_app();
    let workdir = app.current_workdir().to_string_lossy().to_string();
    app.db
        .insert_last_prompt(
            "lp-1",
            &workdir,
            "recovered prompt",
            None,
            chrono::Utc::now(),
        )
        .expect("seed last prompt");

    app.open_simple_prompt_dialog(None);
    assert!(app.simple_prompt_dialog.as_ref().unwrap().is_empty());

    ctrl_l(&mut app);

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_none());
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "recovered prompt"
    );
}

#[test]
fn ctrl_l_on_a_non_empty_builder_stages_a_confirm_instead_of_overwriting() {
    let (mut app, _dir) = test_app();
    let workdir = app.current_workdir().to_string_lossy().to_string();
    app.db
        .insert_last_prompt(
            "lp-1",
            &workdir,
            "recovered prompt",
            None,
            chrono::Utc::now(),
        )
        .expect("seed last prompt");

    app.open_simple_prompt_dialog(None);
    app.simple_prompt_dialog
        .as_mut()
        .unwrap()
        .set_section_content("instruction_1", "work in progress".to_string());

    ctrl_l(&mut app);

    // Content must survive until the confirm is answered.
    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_some());
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "work in progress"
    );

    // 'n' cancels the recall and leaves the in-progress draft intact.
    press(&mut app, KeyCode::Char('n'));
    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_none());
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "work in progress"
    );

    // Recall again and this time confirm with 'y'.
    ctrl_l(&mut app);
    press(&mut app, KeyCode::Char('y'));
    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_none());
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "recovered prompt"
    );
}

/// Full round trip through the DB: compose a structured prompt (multiple
/// section types, not just one instruction), persist it the way
/// `submit_prompt`/`schedule_send_prompt` do (rendered text +
/// `PersistedBuilderState` JSON), then recall it into a fresh builder and
/// assert the structure — not a flattened blob — comes back.
#[test]
fn ctrl_l_restores_the_full_structured_view_not_just_flat_text() {
    use crate::tui::app::dialog::PersistedBuilderState;

    let (mut app, _dir) = test_app();
    let workdir = app.current_workdir().to_string_lossy().to_string();

    app.open_simple_prompt_dialog(None);
    {
        let dialog = app.simple_prompt_dialog.as_mut().unwrap();
        dialog.set_section_content("instruction_1", "primary task".to_string());
        dialog.add_section_with_content("goal", "ship the feature".to_string());
        dialog.add_section_with_content("resources", "@src/main.rs".to_string());
    }
    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    let snapshot = PersistedBuilderState::from_dialog(dialog);
    let json = serde_json::to_string(&snapshot).unwrap();

    app.db
        .insert_last_prompt(
            "lp-1",
            &workdir,
            "flattened rendered text",
            Some(&json),
            chrono::Utc::now(),
        )
        .expect("persist structured last prompt");

    // Close and reopen fresh, as if the builder had been closed and the
    // user came back later to edit it again.
    app.discard_simple_prompt_dialog();
    app.open_simple_prompt_dialog(None);
    assert!(app.simple_prompt_dialog.as_ref().unwrap().is_empty());

    ctrl_l(&mut app);

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_none());
    // The structure survived: three distinct sections, not one
    // instruction section holding the flattened rendered text.
    assert_eq!(dialog.get_section_content("instruction_1"), "primary task");
    assert_eq!(dialog.get_section_content("goal_1"), "ship the feature");
    assert_eq!(dialog.get_section_content("resources_1"), "@src/main.rs");
    assert!(dialog.enabled_sections.contains(&"goal_1".to_string()));
    assert!(dialog.enabled_sections.contains(&"resources_1".to_string()));
    // None of the sections regressed to the flattened text.
    for id in &dialog.enabled_sections {
        assert_ne!(dialog.get_section_content(id), "flattened rendered text");
    }
}

/// A row written before structured persistence existed (`builder_state`
/// is `NULL`) must still open — in raw, as a single instruction section
/// — never error or panic.
#[test]
fn ctrl_l_on_a_pre_migration_row_with_no_builder_state_opens_in_raw_without_error() {
    let (mut app, _dir) = test_app();
    let workdir = app.current_workdir().to_string_lossy().to_string();

    app.db
        .insert_last_prompt(
            "lp-legacy",
            &workdir,
            "an old prompt saved before structure was tracked",
            None,
            chrono::Utc::now(),
        )
        .expect("seed legacy last prompt");

    app.open_simple_prompt_dialog(None);
    ctrl_l(&mut app);

    let dialog = app.simple_prompt_dialog.as_ref().unwrap();
    assert!(dialog.pending_recall.is_none());
    assert_eq!(
        dialog.get_section_content("instruction_1"),
        "an old prompt saved before structure was tracked"
    );
    // Raw fallback is a single section — no structure to have restored.
    assert_eq!(dialog.enabled_sections, vec!["instruction_1".to_string()]);
}

/// A scheduled send confirmed from the Raw tab persists NO structured
/// state: the raw buffer is what gets delivered, so the stale sections are
/// discarded and reopening the row lands back in Raw with the flat text
/// (spec CP2: raw wins when it is out of sync with the structure).
#[test]
fn scheduling_from_the_raw_tab_discards_structure_and_reopens_in_raw() {
    use crate::tui::app::dialog::{PromptTab, SimplePromptDialog, RAW_SECTION_ID};

    let (mut app, _dir) = test_app();
    app.open_simple_prompt_dialog(None);
    {
        let dialog = app.simple_prompt_dialog.as_mut().unwrap();
        dialog.set_section_content("instruction_1", "stale structured leftover".to_string());
        dialog.set_tab(PromptTab::Raw);
        dialog.set_section_content(RAW_SECTION_ID, "raw text that wins".to_string());
    }

    let when = (chrono::Local::now() + chrono::Duration::hours(1)).naive_local();
    super::schedule_send_prompt(&mut app, "raw text that wins", when);

    let rows = app
        .db
        .list_due_scheduled_sends(chrono::Utc::now() + chrono::Duration::hours(2))
        .unwrap();
    let row = rows
        .iter()
        .find(|s| s.prompt == "raw text that wins")
        .expect("scheduled row persisted");
    assert!(
        row.builder_state.is_none(),
        "a raw-tab schedule must not persist structured state"
    );

    // Reopening lands in Raw with the flat text — never the stale section.
    let mut fresh = SimplePromptDialog::new();
    let fire_local = row.fire_at.with_timezone(&chrono::Local).naive_local();
    fresh.load_scheduled_for_edit(
        &row.id,
        &row.prompt,
        fire_local,
        row.builder_state.as_deref(),
    );
    assert_eq!(fresh.active_tab, PromptTab::Raw);
    assert_eq!(fresh.raw_text(), "raw text that wins");
}
