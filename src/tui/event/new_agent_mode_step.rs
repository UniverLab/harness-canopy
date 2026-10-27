//! Keys for the new-session dialog's Mode step (CT34): the choice between
//! `New session` and `Resume a session`, and the session list that Resume
//! opens. Delegates list navigation to the existing pickers' handlers so
//! there is no third list widget to keep in sync.

use anyhow::Result;
use ratatui::crossterm::event::KeyCode;

use crate::tui::app::dialog::new_agent::mode_step::{MODE_ROW_COUNT, NEW_ROW, RESUME_ROW};
use crate::tui::app::dialog::new_agent::{DialogStep, NewAgentDialog, NewTaskMode};
use crate::tui::app::session_resume::SessionResumePicker;
use crate::tui::app::types::App;

use super::new_agent_dialog::{
    confirm_session_resume_picker, handle_session_picker_key, handle_session_resume_picker_key,
};

/// Handle one key while the Mode step is showing. `None` of the form's key
/// handling runs until the step hands control back (FR 2).
pub(crate) fn handle_mode_step_key(app: &mut App, code: KeyCode) -> Result<()> {
    let launch = {
        let Some(dialog) = app.new_agent_dialog.as_mut() else {
            return Ok(());
        };
        step_key(dialog, code)
    };

    if launch {
        let _ = app.launch_new_agent();
    }
    Ok(())
}

/// One Mode-step keystroke; `true` means the confirmed choice must launch
/// now (New session, or a session confirmed from an open list).
fn step_key(dialog: &mut NewAgentDialog, code: KeyCode) -> bool {
    if list_open(dialog) {
        return list_key(dialog, code);
    }
    choice_key(dialog, code)
}

fn list_open(dialog: &NewAgentDialog) -> bool {
    dialog.session_resume_picker.is_some() || dialog.session_picker_open
}

/// A list is open: delegate navigation/confirm/cancel to the picker
/// handlers the form's Resume path already used. Enter confirmed a session,
/// so it launches resumed with `build_resume_args`' output — no mission
/// step (FR 4). Esc just closes the list, back on the choice.
fn list_key(dialog: &mut NewAgentDialog, code: KeyCode) -> bool {
    if matches!(code, KeyCode::Enter) {
        if dialog.session_resume_picker.is_some() {
            confirm_session_resume_picker(dialog);
        } else {
            dialog.confirm_session_pick();
        }
        dialog.task_mode = NewTaskMode::Resume;
        return true;
    }

    if dialog.session_resume_picker.is_some() {
        handle_session_resume_picker_key(dialog, code);
    } else {
        handle_session_picker_key(dialog, code);
    }
    false
}

/// The New/Resume choice itself: ↑↓ move, Enter confirm, Esc back to the
/// form — with every form value kept (FR 2).
fn choice_key(dialog: &mut NewAgentDialog, code: KeyCode) -> bool {
    match code {
        KeyCode::Up => {
            move_mode_index(dialog, false);
            false
        }
        KeyCode::Down => {
            move_mode_index(dialog, true);
            false
        }
        KeyCode::Esc => {
            dialog.step = DialogStep::Form;
            false
        }
        KeyCode::Enter if dialog.mode_index == NEW_ROW => {
            // New session: exactly today's Interactive create path (FR 3).
            dialog.step = DialogStep::Form;
            dialog.task_mode = NewTaskMode::Interactive;
            true
        }
        KeyCode::Enter if dialog.mode_index == RESUME_ROW => open_resume_list(dialog),
        _ => false,
    }
}

fn move_mode_index(dialog: &mut NewAgentDialog, forward: bool) {
    dialog.mode_index =
        crate::tui::selection::move_index(dialog.mode_index, MODE_ROW_COUNT, forward);
}

/// Resume: show the sessions that apply to the form's harness and directory
/// — the CLI's own list when the harness has a `session_list_cmd`, the
/// native candidates otherwise (FR 4). With nothing to resume this is a
/// no-op; `New session` stays selectable (FR 5). The dialog is marked
/// `Resume` only once a session is actually confirmed (FR 4).
fn open_resume_list(dialog: &mut NewAgentDialog) -> bool {
    dialog.reset_resume_choice();

    if dialog.has_cli_session_list() {
        // `open_session_picker` only opens when the CLI listed something.
        dialog.open_session_picker();
        return false;
    }

    if dialog.mode_candidates.is_empty() {
        return false;
    }

    dialog.session_resume_picker = Some(SessionResumePicker::new(
        dialog
            .mode_candidates
            .iter()
            .map(crate::tui::app::session_resume::ResumableSession::from)
            .collect(),
    ));
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog_with_cli() -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some("/tmp/proj"));
        d.available_clis = vec![
            crate::domain::models::Cli::new("claude"),
            crate::domain::models::Cli::new("codex"),
        ];
        d.cli_configs = vec![None, None];
        d.cli_index = 0;
        d.working_dir = "/tmp/proj".to_string();
        d
    }

    fn session(id: &str, cli: &str, dir: &str) -> crate::db::session::InteractiveSession {
        crate::db::session::InteractiveSession {
            id: id.to_string(),
            name: format!("session-{id}"),
            cli: cli.to_string(),
            working_dir: dir.to_string(),
            args: None,
            started_at: "2026-10-01T09:00:00Z".to_string(),
            status: "completed".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
            harness_conversation_id: None,
        }
    }

    #[test]
    fn confirming_the_native_list_marks_resume_and_keeps_the_session() {
        let mut d = dialog_with_cli();
        crate::tui::app::dialog::new_agent::mode_step::open_mode_step(
            &mut d,
            vec![session("s1", "claude", "/tmp/proj")],
        );
        d.mode_index = RESUME_ROW;
        assert!(
            !choice_key(&mut d, KeyCode::Enter),
            "opening the list is not a launch"
        );
        assert!(d.session_resume_picker.is_some());
        assert_eq!(
            d.task_mode,
            NewTaskMode::Interactive,
            "Resume is marked only when a session is confirmed"
        );

        assert!(list_key(&mut d, KeyCode::Enter), "confirming must launch");
        assert!(d.session_resume_picker.is_none());
        assert_eq!(d.task_mode, NewTaskMode::Resume);
        assert_eq!(
            d.selected_resume_session.as_ref().map(|s| s.id.as_str()),
            Some("s1")
        );
    }

    #[test]
    fn new_session_choice_returns_true_without_touching_resume_state() {
        let mut d = dialog_with_cli();
        assert!(choice_key(&mut d, KeyCode::Enter));
        assert_eq!(d.step, DialogStep::Form);
        assert_eq!(d.task_mode, NewTaskMode::Interactive);
        assert!(d.selected_resume_session.is_none());
    }
}
