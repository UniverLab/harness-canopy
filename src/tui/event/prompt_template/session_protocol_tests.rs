use super::submit_prompt;
use crate::application::ports::StateRepository;
use crate::db::Database;
use crate::tui::app::dialog::{PromptTab, SimplePromptDialog, RAW_SECTION_ID};
use crate::tui::app::types::{AgentEntry, App};
use crate::tui::app::SidebarLayer;
use std::path::Path;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

const FIRST_ACTION: &str = "get_tools(scope=\"session_start\")";

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn app_with_db(db: Arc<Database>) -> (App, tempfile::TempDir) {
    let data_dir = tempdir().expect("create data dir");
    let app = App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    (app, data_dir)
}

fn test_app() -> (App, tempfile::TempDir) {
    app_with_db(test_db())
}

/// A `cat` PTY standing in for a real harness session (same pattern as
/// `ui::dialogs::simple_prompt`'s `app_for_prompt`), selected so the
/// prompt session key resolves to `interactive:<id>`.
fn attach_session(app: &mut App) -> String {
    let workdir = app.current_workdir().to_string_lossy().to_string();
    let agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
        "cat",
        &workdir,
        80,
        24,
        Some("session-under-test"),
        &[],
        ratatui::style::Color::Reset,
    )
    .expect("spawn cat as a stand-in harness session");
    app.interactive_agents.push(agent);
    app.agents.push(AgentEntry::Interactive(0));
    app.selected = 0;
    app.interactive_agents[0].id.clone()
}

/// Like [`attach_session`], but the spawned agent takes the given id —
/// how a test simulates the same session id surviving a restart.
fn attach_session_with_id(app: &mut App, id: &str) -> String {
    let _ = attach_session(app);
    app.interactive_agents[0].id = id.to_string();
    id.to_string()
}

fn kill_sessions(app: &mut App) {
    for agent in app.interactive_agents.iter_mut() {
        agent.kill();
    }
}

fn protocol_included(app: &App) -> bool {
    app.simple_prompt_dialog
        .as_ref()
        .map(|d| d.protocol_included)
        .expect("prompt dialog open")
}

fn compose_prompt(app: &App) -> String {
    let workdir = app.current_workdir();
    app.simple_prompt_dialog
        .as_ref()
        .expect("prompt dialog open")
        .build_prompt_with_resolved_resources(&app.db, &workdir)
        .expect("compose prompt")
}

fn addable_names(dialog: &SimplePromptDialog) -> Vec<&str> {
    dialog
        .get_addable_sections()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The system block the dialog would actually send this turn — the
/// protocol once at the top when it rides along, then the per-turn
/// context (i.e. what the agent really saw).
fn system_text(app: &App) -> String {
    app.simple_prompt_dialog
        .as_ref()
        .map(|d| d.system_text())
        .unwrap_or_default()
}

/// Opens the dialog, submits `prompt`, and returns the system block
/// that was assembled for that turn (i.e. what the agent actually saw).
fn send_turn(app: &mut App, prompt: &str) -> String {
    app.open_simple_prompt_dialog(None);
    let content = system_text(app);
    submit_prompt(app, prompt);
    content
}

#[test]
fn turn_one_of_a_session_contains_the_session_start_block() {
    let (mut app, _dir) = test_app();

    let turn_1 = send_turn(&mut app, "do the first thing");

    assert!(turn_1.contains("[CANOPY]"));
    assert!(turn_1.contains(FIRST_ACTION));
}

#[test]
fn turn_two_of_the_same_session_omits_the_session_start_block() {
    let (mut app, _dir) = test_app();

    send_turn(&mut app, "do the first thing");
    let turn_2 = send_turn(&mut app, "do the second thing");

    assert!(!turn_2.contains("[CANOPY]"));
    assert!(!turn_2.contains(FIRST_ACTION));
}

#[test]
fn per_turn_workspace_context_is_present_on_every_turn() {
    let (mut app, _dir) = test_app();

    let turn_1 = send_turn(&mut app, "do the first thing");
    let turn_2 = send_turn(&mut app, "do the second thing");
    let turn_3 = send_turn(&mut app, "do the third thing");

    assert!(turn_1.contains("workspace:"));
    assert!(turn_2.contains("workspace:"));
    assert!(turn_3.contains("workspace:"));
}

#[test]
fn a_new_session_after_the_previous_one_ended_gets_the_block_again() {
    let (mut app, _dir) = test_app();
    send_turn(&mut app, "do the first thing");

    // The previous session's process ends; a brand new one starts with
    // no shared in-memory state (a fresh `App`, per the documented
    // decision that a session's protocol-delivery state does not
    // outlive the process it was tracked in).
    let (mut new_session_app, _dir2) = test_app();
    let new_turn_1 = send_turn(&mut new_session_app, "do the first thing, again");

    assert!(new_turn_1.contains("[CANOPY]"));
    assert!(new_turn_1.contains(FIRST_ACTION));
}

/// The protocol is gated solely by `protocol_sent` now — the solo/sync
/// mode of the session no longer feeds into the decision (the
/// `sent_as_solo`/`sync_available` re-injection path was removed). Once
/// turn 1 delivers it, no later turn re-arms it, and `submit_prompt`
/// records the delivery on `session_protocol_state`.
#[test]
fn protocol_is_gated_only_by_protocol_sent_not_by_session_mode() {
    let (mut app, _dir) = test_app();

    app.open_simple_prompt_dialog(None);
    assert!(
        app.simple_prompt_dialog.as_ref().unwrap().protocol_included,
        "turn 1 must include the protocol"
    );
    submit_prompt(&mut app, "do the first thing");

    let key = app.current_prompt_session_key();
    assert!(
        app.session_protocol_state
            .get(&key)
            .is_some_and(|s| s.protocol_sent),
        "submitting turn 1 must record protocol_sent"
    );

    // No per-turn condition remains that could re-inject the block.
    let turn_2 = send_turn(&mut app, "do the second thing");
    assert!(!turn_2.contains("[CANOPY]"));
    let turn_3 = send_turn(&mut app, "do the third thing");
    assert!(!turn_3.contains("[CANOPY]"));
}

#[test]
fn first_action_phrase_appears_at_most_once_across_a_ten_turn_session() {
    let (mut app, _dir) = test_app();

    let turns: Vec<String> = (0..10)
        .map(|i| send_turn(&mut app, &format!("turn {i}")))
        .collect();

    let occurrences = turns
        .iter()
        .filter(|content| content.contains(FIRST_ACTION))
        .count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one turn to carry the session-start block"
    );
}

/// FR5: measures the assembled system-block size for a ten-turn session,
/// comparing a hypothetical baseline where the protocol block is repeated
/// every turn against the actual behavior (protocol block sent once,
/// per-turn context sent every turn).
#[test]
fn ten_turn_session_sends_far_fewer_protocol_tokens_than_resending_every_turn() {
    let (mut app, _dir) = test_app();

    let turns: Vec<String> = (0..10)
        .map(|i| send_turn(&mut app, &format!("turn {i}")))
        .collect();

    let turn_1_chars = turns[0].chars().count();
    let turn_2_chars = turns[1].chars().count();
    let after_chars: usize = turns.iter().map(|t| t.chars().count()).sum();
    // Baseline this replaces: the full block (protocol + per-turn
    // context) repeated on all ten turns, as observed in the bug report.
    let before_chars = turn_1_chars * 10;

    assert!(
        turn_1_chars > turn_2_chars,
        "turn 1 must carry more than later turns"
    );
    assert!(
        after_chars < before_chars,
        "after ({after_chars} chars) should be well below before ({before_chars} chars)"
    );
    // ~4 chars/token is a standard rough estimator; exact tokenization is
    // beside the point here — the ratio is what the fix targets.
    let before_tokens = before_chars / 4;
    let after_tokens = after_chars / 4;
    assert!(
        after_tokens * 2 < before_tokens,
        "expected at least a 2x reduction in estimated tokens over ten turns: \
             before={before_tokens} after={after_tokens}"
    );
}

/// FR6/T1: a session whose persisted key exists does not get the
/// protocol on a fresh `App` — the restart case, where the in-memory
/// map starts empty but `daemon_state` still says the block went out.
#[test]
fn persisted_key_suppresses_the_protocol_on_a_fresh_app() {
    let db = test_db();
    let (mut app, _dir) = app_with_db(Arc::clone(&db));
    let session_id = attach_session(&mut app);

    app.open_simple_prompt_dialog(None);
    assert!(
        protocol_included(&app),
        "the first turn includes the protocol"
    );
    submit_prompt(&mut app, "first turn");
    assert!(
        db.get_state(&format!("prompt_protocol_sent:{session_id}"))
            .expect("read daemon_state")
            .is_some(),
        "the send must persist the spec's daemon_state key"
    );
    kill_sessions(&mut app);

    // Restart: a fresh App over the same database, same session id.
    let (mut restarted, _dir2) = app_with_db(db);
    attach_session_with_id(&mut restarted, &session_id);
    restarted.open_simple_prompt_dialog(None);
    assert!(
        !protocol_included(&restarted),
        "a restored session with a persisted flag must not receive the protocol again"
    );
    assert!(
        restarted.session_protocol_state.is_empty(),
        "a fresh App starts with no in-memory protocol state"
    );
    kill_sessions(&mut restarted);
}

/// FR6/T2: a session resumed under a NEW id (a new uuid per spawn)
/// inherits the source's persisted flag, and the new id's dialog does
/// not send the protocol. An existing target flag is never overwritten.
#[test]
fn resumed_session_with_a_new_id_inherits_the_persisted_flag() {
    let (mut app, _dir) = test_app();
    app.db
        .set_state("prompt_protocol_sent:old-session-id", "1234567890")
        .expect("seed source flag");
    app.db
        .set_state("prompt_protocol_sent:already-flagged", "42")
        .expect("seed target flag");

    app.inherit_protocol_flag("old-session-id", "fresh-uuid-id");
    assert_eq!(
        app.db
            .get_state("prompt_protocol_sent:fresh-uuid-id")
            .unwrap()
            .as_deref(),
        Some("1234567890"),
        "the source's persisted value must be inherited verbatim"
    );

    app.inherit_protocol_flag("old-session-id", "already-flagged");
    assert_eq!(
        app.db
            .get_state("prompt_protocol_sent:already-flagged")
            .unwrap()
            .as_deref(),
        Some("42"),
        "an existing target flag wins over inheritance"
    );

    attach_session_with_id(&mut app, "fresh-uuid-id");
    app.open_simple_prompt_dialog(None);
    assert!(
        !protocol_included(&app),
        "the inherited flag suppresses the block for the new id"
    );
    kill_sessions(&mut app);
}

/// FR6/T3: the `Canopy protocol` section added to a session that
/// already had the block sends it exactly once at the top of the
/// prompt (before the per-turn context, inside the system block, out
/// of the body), is locked, and records the persisted key on send.
#[test]
fn canopy_protocol_section_resends_once_at_the_top_and_records_the_key() {
    let (mut app, _dir) = test_app();
    let session_id = attach_session(&mut app);

    // The block went out earlier in this process: in-memory state only.
    let session_key = format!("interactive:{session_id}");
    app.session_protocol_state
        .entry(session_key)
        .or_default()
        .protocol_sent = true;

    app.open_simple_prompt_dialog(None);
    let dialog = app
        .simple_prompt_dialog
        .as_mut()
        .expect("prompt dialog open");
    assert!(
        !dialog.protocol_included,
        "the automatic path is spent for this session"
    );
    dialog.add_canopy_protocol_section();
    let section_id = dialog
        .enabled_sections
        .last()
        .cloned()
        .expect("the section was added");
    assert!(
        dialog.is_locked(&section_id),
        "the section must be read-only"
    );
    assert_eq!(
        dialog.get_section_content(&section_id),
        crate::domain::sandbox::canopy_protocol_block(),
        "the section content must be the canonical block, never a copy"
    );
    assert!(
        dialog.sends_protocol(),
        "the section counts as 'protocol included'"
    );

    let prompt = compose_prompt(&app);
    assert_eq!(
        prompt.matches("[CANOPY]").count(),
        1,
        "the block must appear exactly once"
    );
    let canopy_at = prompt.find("[CANOPY]").expect("block present");
    let workspace_at = prompt.find("workspace:").expect("per-turn context present");
    assert!(
        canopy_at < workspace_at,
        "block first, per-turn context after"
    );
    let system_end = prompt.find("</system>").expect("system block present");
    assert!(
        canopy_at < system_end,
        "the block rides in the system block, never in the body"
    );

    submit_prompt(&mut app, "resend the protocol");
    assert!(
        app.protocol_persisted(&session_id),
        "a section-driven resend must record the persisted key (FR1)"
    );
    kill_sessions(&mut app);
}

/// FR4: even when the automatic path would also have added the block
/// (first turn, `protocol_included`), the manual section must not
/// duplicate it — one block, at the top.
#[test]
fn automatic_flag_and_manual_section_send_the_block_exactly_once() {
    let (mut app, _dir) = test_app();
    let _session_id = attach_session(&mut app);

    app.open_simple_prompt_dialog(None);
    let dialog = app
        .simple_prompt_dialog
        .as_mut()
        .expect("prompt dialog open");
    assert!(dialog.protocol_included, "the first turn is automatic");
    dialog.add_canopy_protocol_section();

    let prompt = compose_prompt(&app);
    assert_eq!(prompt.matches("[CANOPY]").count(), 1, "never twice");
    let canopy_at = prompt.find("[CANOPY]").expect("block present");
    let workspace_at = prompt.find("workspace:").expect("per-turn context present");
    assert!(
        canopy_at < workspace_at,
        "block first, per-turn context after"
    );
    kill_sessions(&mut app);
}

/// FR6/T4: a project-keyed prompt keeps today's in-memory behaviour —
/// recorded in memory, with no `daemon_state` key written at all.
#[test]
fn project_keyed_prompt_records_only_in_memory() {
    let (mut app, data_dir) = test_app();
    let project_dir = data_dir.path().join("project");
    std::fs::create_dir(&project_dir).expect("create project dir");
    let project = app
        .db
        .register_project_path(&project_dir)
        .expect("register project");
    app.projects.push(project);
    app.selected_project = app.projects.len() - 1;
    app.sidebar_layer = SidebarLayer::Knowledge;

    let session_key = app.current_prompt_session_key();
    assert!(
        session_key.starts_with("project:"),
        "expected a project key, got {session_key}"
    );

    app.open_simple_prompt_dialog(None);
    assert!(
        protocol_included(&app),
        "the first project turn includes the protocol"
    );
    submit_prompt(&mut app, "project turn");

    assert!(
        app.session_protocol_state
            .get(&session_key)
            .is_some_and(|s| s.protocol_sent),
        "the in-memory record is kept for project keys"
    );
    let project_path = app.projects[app.selected_project].path.clone();
    assert!(
        !app.protocol_persisted(&project_path),
        "no daemon_state key is written for a non-interactive session"
    );
}

/// FR6/T5: a verbatim Raw send carries no protocol and records nothing —
/// neither the in-memory flag nor the persisted key.
#[test]
fn raw_verbatim_send_records_no_protocol_flag() {
    let (mut app, _dir) = test_app();
    let session_id = attach_session(&mut app);

    app.open_simple_prompt_dialog(None);
    assert!(
        protocol_included(&app),
        "the dialog is armed for the first turn"
    );
    {
        let dialog = app
            .simple_prompt_dialog
            .as_mut()
            .expect("prompt dialog open");
        dialog.set_tab(PromptTab::Raw);
        dialog.set_section_content(RAW_SECTION_ID, "/compact".to_string());
        assert!(!dialog.sends_protocol(), "a raw send carries no protocol");
    }
    submit_prompt(&mut app, "/compact");

    let session_key = format!("interactive:{session_id}");
    assert!(
        !app.session_protocol_state
            .get(&session_key)
            .is_some_and(|s| s.protocol_sent),
        "a raw send must not record the in-memory flag"
    );
    assert!(
        !app.protocol_persisted(&session_id),
        "a raw send must not write the persisted key"
    );
    kill_sessions(&mut app);
}

/// FR6/T6: the add-section picker offers `Canopy protocol` only while
/// it is not enabled; the enabled section is listed as removable and
/// removing it re-offers the section.
#[test]
fn canopy_protocol_section_is_offered_once_and_removable() {
    let mut dialog = SimplePromptDialog::new();
    assert!(
        SimplePromptDialog::get_available_sections()
            .iter()
            .any(|(name, label)| *name == "canopy_protocol" && *label == "Canopy protocol"),
        "the registry offers the section under its exact name and label"
    );
    assert!(
        addable_names(&dialog).contains(&"canopy_protocol"),
        "offered while not enabled"
    );

    dialog.add_canopy_protocol_section();
    assert!(
        !addable_names(&dialog).contains(&"canopy_protocol"),
        "single-instance: hidden from the picker once enabled"
    );
    let section_id = dialog
        .get_removable_sections()
        .into_iter()
        .map(|(id, _)| id)
        .find(|id| id.starts_with("canopy_protocol"))
        .expect("the enabled section stays removable");

    dialog.remove_section(&section_id);
    assert!(
        addable_names(&dialog).contains(&"canopy_protocol"),
        "offered again after removal"
    );
}

/// FR6/T6 (picker arm): selecting the row in the actual add-section
/// picker inserts the locked, pre-filled section and closes the picker.
#[test]
fn selecting_canopy_protocol_in_the_picker_inserts_the_locked_section() {
    let mut dialog = SimplePromptDialog::new();
    let db = test_db();
    let idx = dialog
        .get_addable_sections()
        .iter()
        .position(|(name, _)| *name == "canopy_protocol")
        .expect("offered in the add-section list");

    super::section_picker::select_addable_section(&mut dialog, idx, &db, Path::new("/tmp"))
        .expect("selection succeeds");

    assert!(matches!(
        dialog.picker_mode,
        crate::tui::app::dialog::SectionPickerMode::None
    ));
    let section_id = dialog
        .enabled_sections
        .last()
        .cloned()
        .expect("the section was added");
    assert!(section_id.starts_with("canopy_protocol"));
    assert!(
        dialog.is_locked(&section_id),
        "the section must be read-only"
    );
    assert_eq!(
        dialog.get_section_content(&section_id),
        crate::domain::sandbox::canopy_protocol_block(),
        "filled with the canonical block"
    );
}
