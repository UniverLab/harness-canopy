//! The Mode step of the new-session dialog (CT34): two rows — `New session`
//! and `Resume a session` — plus the session list Resume opens. Shares the
//! pickers' row builders so the list looks and scrolls like every other list.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::picker_item_style;
use super::pickers::{append_session_picker_rows, append_session_resume_picker_rows};
use crate::tui::app::dialog::new_agent::mode_step::{self, NEW_ROW, RESUME_ROW};
use crate::tui::app::dialog::new_agent::NewAgentDialog;
use crate::tui::ui::theme::Theme;

/// Footer of the Mode step (FR 6).
const FOOTER: &str = "  ↑↓ select · Enter confirm · Esc back";

/// Render the Mode step's lines into the dialog block (the caller draws the
/// frame and title).
pub(super) fn build_dialog_lines(dialog: &NewAgentDialog, theme: &Theme) -> Vec<Line<'static>> {
    let accent = theme.header_color;
    let mut lines = vec![Line::from("")];

    lines.push(choice_row(
        "New session",
        dialog.mode_index == NEW_ROW,
        accent,
    ));
    lines.push(choice_row(
        &resume_row_label(dialog),
        dialog.mode_index == RESUME_ROW,
        accent,
    ));
    lines.push(Line::from(""));

    // The list Resume opened, reusing the existing picker rows (FR 4).
    append_session_resume_picker_rows(&mut lines, dialog, theme);
    append_session_picker_rows(&mut lines, dialog, theme);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        FOOTER,
        Style::default().fg(theme.dim_text),
    )));
    lines
}

/// Render one selectable choice row: a teal (CT33) selection highlight when
/// it is the current row.
fn choice_row(label: &str, selected: bool, accent: Color) -> Line<'static> {
    let style = picker_item_style(accent, selected);
    Line::from(vec![
        Span::styled(format!("    {} ", if selected { "›" } else { " " }), style),
        Span::styled(label.to_string(), style),
    ])
}

/// The Resume row's label: the choice itself, or — when neither native
/// candidates nor a CLI session list can offer anything — the empty message
/// naming the harness and directory it looked at (FR 5).
fn resume_row_label(dialog: &NewAgentDialog) -> String {
    if !mode_step::resume_row_is_empty(dialog) {
        return "Resume a session".to_string();
    }
    format!(
        "No sessions to resume for {} in {}",
        dialog.selected_cli().as_str(),
        dialog.working_dir
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::dialog::new_agent::DialogStep;

    fn mode_dialog(dir: &str) -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some(dir));
        d.task_type = crate::tui::app::dialog::new_agent::NewTaskType::Interactive;
        d.available_clis = vec![
            crate::domain::models::Cli::new("claude"),
            crate::domain::models::Cli::new("codex"),
        ];
        d.cli_configs = vec![None, None];
        d.cli_index = 0;
        d.working_dir = dir.to_string();
        d.step = DialogStep::Mode;
        d
    }

    fn lines_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
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
    fn shows_both_choices_and_the_footer() {
        let mut d = mode_dialog("/tmp/proj");
        mode_step::open_mode_step(&mut d, vec![session("s1", "claude", "/tmp/proj")]);
        let text = lines_text(&build_dialog_lines(&d, &Theme::classic()));
        assert!(text.contains("New session"), "{text}");
        assert!(text.contains("Resume a session"), "{text}");
        assert!(text.contains(FOOTER.trim()), "{text}");
    }

    #[test]
    fn empty_resume_row_names_the_harness_and_directory() {
        let mut d = mode_dialog("/tmp/proj");
        mode_step::open_mode_step(&mut d, vec![session("s1", "codex", "/tmp/proj")]);

        let text = lines_text(&build_dialog_lines(&d, &Theme::classic()));
        assert!(
            text.contains("No sessions to resume for claude in /tmp/proj"),
            "{text}"
        );
    }

    #[test]
    fn resume_row_is_plain_when_there_are_candidates() {
        let mut d = mode_dialog("/tmp/proj");
        mode_step::open_mode_step(&mut d, vec![session("s1", "claude", "/tmp/proj")]);

        let text = lines_text(&build_dialog_lines(&d, &Theme::classic()));
        assert!(text.contains("Resume a session"), "{text}");
        assert!(!text.contains("No sessions to resume"), "{text}");
    }

    #[test]
    fn open_native_list_adds_its_rows() {
        let mut d = mode_dialog("/tmp/proj");
        mode_step::open_mode_step(
            &mut d,
            vec![
                session("s1", "claude", "/tmp/proj"),
                session("s2", "claude", "/tmp/proj"),
            ],
        );
        d.mode_index = RESUME_ROW;
        d.session_resume_picker = Some(crate::tui::app::session_resume::SessionResumePicker::new(
            d.mode_candidates
                .iter()
                .map(crate::tui::app::session_resume::ResumableSession::from)
                .collect(),
        ));

        let text = lines_text(&build_dialog_lines(&d, &Theme::classic()));
        assert!(text.contains("session-s1"), "{text}");
        assert!(text.contains("session-s2"), "{text}");
    }

    #[test]
    fn choice_rows_use_the_teal_selection_background() {
        let d = mode_dialog("/tmp/proj");
        let theme = Theme::classic();
        let accent = theme.header_color;
        // New session is the selected row by default: its cells carry the
        // canopy teal behind them; the Resume row does not.
        let lines = build_dialog_lines(&d, &theme);
        let new_row = &lines[1];
        assert!(new_row.spans.iter().any(|s| s.style.bg == Some(accent)));
        let resume_row = &lines[2];
        assert!(resume_row.spans.iter().all(|s| s.style.bg != Some(accent)));
    }
}
