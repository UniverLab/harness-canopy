//! The Mode step of the new-session dialog (CT34): the form collects type,
//! harness, directory and the rest; this step only asks New session or
//! Resume a session. Pure state transitions — the key handling lives in
//! [`crate::tui::event::new_agent_mode_step`].

use crate::db::session::InteractiveSession;
use crate::tui::app::dialog::new_agent::{DialogStep, NewAgentDialog, NewTaskMode, NewTaskType};

/// New session row on the Mode step.
pub(crate) const NEW_ROW: usize = 0;
/// Resume a session row on the Mode step.
pub(crate) const RESUME_ROW: usize = 1;
/// Total choice rows on the Mode step.
pub(crate) const MODE_ROW_COUNT: usize = 2;

/// The Mode step follows an Interactive form in create mode, and only then:
/// Terminal and Background types and edit mode go straight
/// from the form to today's launch path (FR 2).
pub(crate) fn can_open_mode_step(dialog: &NewAgentDialog) -> bool {
    matches!(dialog.task_type, NewTaskType::Interactive) && !dialog.is_edit_mode()
}

/// Enter the Mode step with the resumable sessions carried over from the DB.
/// Any stale resume state from a previous pass is cleared and the choice
/// resets to `New session`.
pub(crate) fn open_mode_step(dialog: &mut NewAgentDialog, sessions: Vec<InteractiveSession>) {
    dialog.task_mode = NewTaskMode::Interactive;
    dialog.reset_resume_choice();
    dialog.selected_session = None;
    dialog.session_picker_open = false;
    dialog.mode_candidates = filter_candidates(dialog, sessions);
    dialog.mode_index = NEW_ROW;
    dialog.step = DialogStep::Mode;
}

/// Sessions resumable for the harness and directory chosen on Step 2/Step 1:
/// finished (`status != "active"` and not live in the hub), ordered by
/// `started_at DESC`, deduplicated by non-empty `harness_conversation_id`
/// (keeping the newest row), and capped at 20.
fn filter_candidates(
    dialog: &NewAgentDialog,
    sessions: Vec<InteractiveSession>,
) -> Vec<InteractiveSession> {
    let harness = dialog.selected_cli().as_str().to_string();
    let dir = dialog.working_dir.trim_end_matches('/').to_string();
    let current_boot_id = crate::system::boot_id();

    let mut filtered: Vec<InteractiveSession> = sessions
        .into_iter()
        .filter(|session| {
            session.status != "active"
                && !crate::system::session_process_is_live(
                    session.pid,
                    session.boot_id.as_deref(),
                    current_boot_id.as_deref(),
                )
                && session.cli == harness
                && session.working_dir.trim_end_matches('/') == dir
        })
        .collect();

    filtered.sort_by(|a, b| b.started_at.cmp(&a.started_at));

    let mut seen_conv_ids = std::collections::HashSet::new();
    filtered.retain(|session| {
        let Some(conv_id) = session
            .harness_conversation_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return true;
        };
        seen_conv_ids.insert(conv_id.to_string())
    });

    filtered.truncate(20);
    filtered
}

/// Whether the Mode step's Resume row has nothing to offer: no native
/// candidates, no CLI-owned session list, and no `session_resume_cmd`
/// (which always offers `"other…"`).
pub(crate) fn resume_row_is_empty(dialog: &NewAgentDialog) -> bool {
    dialog.mode_candidates.is_empty()
        && !dialog.has_cli_session_list()
        && !dialog.has_session_resume_cmd()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, cli: &str, working_dir: &str) -> InteractiveSession {
        InteractiveSession {
            id: id.to_string(),
            name: format!("session-{id}"),
            cli: cli.to_string(),
            working_dir: working_dir.to_string(),
            args: None,
            started_at: "2026-10-01T09:00:00Z".to_string(),
            status: "completed".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
            harness_conversation_id: None,
        }
    }

    fn interactive_dialog(dir: &str) -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some(dir));
        d.task_type = NewTaskType::Interactive;
        d.available_clis = vec![
            crate::domain::models::Cli::new("claude"),
            crate::domain::models::Cli::new("codex"),
        ];
        d.cli_configs = vec![None, None];
        d.cli_index = 0;
        d.working_dir = dir.to_string();
        d
    }

    #[test]
    fn opens_only_for_interactive_create_mode() {
        let mut d = interactive_dialog("/tmp/proj");
        assert!(can_open_mode_step(&d));

        d.task_type = NewTaskType::Terminal;
        assert!(!can_open_mode_step(&d), "Terminal skips the step");

        d.task_type = NewTaskType::Background;
        assert!(!can_open_mode_step(&d), "Background skips the step");

        d.task_type = NewTaskType::Interactive;
        d.edit_id = Some("agent-1".to_string());
        assert!(!can_open_mode_step(&d), "edit mode skips the step");
    }

    #[test]
    fn opens_step_filtered_to_harness_and_directory() {
        let mut d = interactive_dialog("/tmp/proj");
        let sessions = vec![
            session("s1", "claude", "/tmp/proj"),
            session("s2", "claude", "/tmp/proj/"), // trailing slash is equal
            session("s3", "codex", "/tmp/proj"),   // other harness
            session("s4", "claude", "/tmp/other"), // other directory
        ];

        open_mode_step(&mut d, sessions);

        assert_eq!(d.step, DialogStep::Mode);
        let ids: Vec<&str> = d.mode_candidates.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2"], "exactly the two matching sessions");
        assert_eq!(d.mode_index, NEW_ROW, "choice starts on New session");
        assert_eq!(d.task_mode, NewTaskMode::Interactive);
    }

    #[test]
    fn reentering_the_step_resets_stale_resume_state() {
        let mut d = interactive_dialog("/tmp/proj");
        open_mode_step(&mut d, vec![]);
        d.task_mode = NewTaskMode::Resume;
        d.selected_session = Some(("stale".into(), "stale".into()));
        d.session_picker_open = true;
        d.step = DialogStep::AgentOptions;

        open_mode_step(&mut d, vec![session("s1", "claude", "/tmp/proj")]);

        assert_eq!(d.task_mode, NewTaskMode::Interactive);
        assert!(d.selected_session.is_none());
        assert!(!d.session_picker_open);
        assert!(d.session_resume_picker.is_none());
    }

    #[test]
    fn resume_row_is_empty_without_candidates_or_cli_list() {
        let mut d = interactive_dialog("/tmp/proj");
        open_mode_step(&mut d, vec![session("s1", "codex", "/tmp/proj")]);
        assert!(
            resume_row_is_empty(&d),
            "no candidate for the chosen harness and no CLI list"
        );

        open_mode_step(&mut d, vec![session("s2", "claude", "/tmp/proj")]);
        assert!(!resume_row_is_empty(&d));
    }

    #[test]
    fn candidates_are_sorted_newest_first_deduped_by_conv_id_and_capped_at_20() {
        let mut d = interactive_dialog("/tmp/proj");
        let mut sessions = Vec::new();
        for i in 0..25 {
            let mut s = session(&format!("s{i:02}"), "claude", "/tmp/proj");
            s.started_at = format!("2026-10-{:02}T09:00:00Z", i + 1);
            sessions.push(s);
        }
        // Two rows sharing a conversation id: only the newer one must survive.
        sessions[0].harness_conversation_id = Some("conv-dup".to_string());
        sessions[24].harness_conversation_id = Some("conv-dup".to_string());
        // Active row must be excluded.
        sessions[23].status = "active".to_string();

        open_mode_step(&mut d, sessions);

        assert_eq!(d.mode_candidates.len(), 20);
        assert_eq!(d.mode_candidates[0].id, "s24");
        assert!(
            !d.mode_candidates.iter().any(|s| s.id == "s23"),
            "active sessions must be excluded"
        );
        assert!(
            !d.mode_candidates.iter().any(|s| s.id == "s00"),
            "older duplicate of conv-dup must be deduplicated"
        );
    }
}
