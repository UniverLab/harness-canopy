//! Mouse handling for the 3-step Ctrl+N `NewAgentDialog`.
//!
//! Uses `app.last_new_agent_inner_rect` recorded by `draw_new_agent_dialog`
//! so hit-testing stays aligned with the rendered layout across all 3 steps.

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use super::handler_errors::report_handler_error;
use super::new_agent_dialog::RESUME_CANDIDATE_FETCH_LIMIT;
use super::new_agent_mode_step::open_resume_list;
use crate::tui::app::dialog::new_agent::mode_step::{self, NEW_ROW, RESUME_ROW};
use crate::tui::app::dialog::{DialogStep, NewAgentDialog, NewTaskMode, NewTaskType};
use crate::tui::app::types::App;
use crate::tui::ui::dialogs::new_agent_dialog::SESSION_RESUME_PICKER_VISIBLE;

/// Handle a mouse event while `Focus::NewAgentDialog` is active.
pub(crate) fn handle_new_agent_dialog_mouse(app: &mut App, mouse: &MouseEvent) {
    let Some(inner) = app.last_new_agent_inner_rect else {
        return;
    };
    if !rect_contains(inner, mouse.column, mouse.row) {
        return;
    }
    let rel_y = (mouse.row - inner.y) as usize;
    let rel_x = (mouse.column - inner.x) as usize;

    let (is_edit, step) = match app.new_agent_dialog.as_ref() {
        Some(d) => (d.is_edit_mode(), d.step),
        None => return,
    };
    if is_edit {
        return;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => match step {
            DialogStep::FolderAndKind => handle_step1_click(app, rel_x, rel_y),
            DialogStep::AgentOptions => handle_step2_click(app, rel_x, rel_y),
            DialogStep::Mode => handle_step3_click(app, rel_y, inner.height as usize),
        },
        MouseEventKind::ScrollUp => handle_scroll(app, step, false),
        MouseEventKind::ScrollDown => handle_scroll(app, step, true),
        _ => {}
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn handle_step1_click(app: &mut App, rel_x: usize, rel_y: usize) {
    enum Step1Action {
        None,
        Advance,
        LaunchTerminal,
    }

    let action = {
        let Some(dialog) = app.new_agent_dialog.as_mut() else {
            return;
        };
        if rel_y == 1 {
            dialog.field = 0;
            // Right side of row 1 holds `[ Next → ]` / `[ Launch → ]` (col >= 32).
            if rel_x >= 32 {
                if matches!(dialog.task_type, NewTaskType::Terminal) {
                    Step1Action::LaunchTerminal
                } else {
                    Step1Action::Advance
                }
            } else {
                dialog.task_type = match dialog.task_type {
                    NewTaskType::Terminal => NewTaskType::Interactive,
                    _ => NewTaskType::Terminal,
                };
                dialog.refresh_dir_entries();
                Step1Action::None
            }
        } else if rel_y == 3 {
            dialog.field = 1;
            Step1Action::None
        } else if rel_y >= 6 {
            click_dir_entry(dialog, rel_y - 6);
            Step1Action::None
        } else {
            Step1Action::None
        }
    };

    match action {
        Step1Action::None => {}
        Step1Action::Advance => {
            if let Some(dialog) = app.new_agent_dialog.as_mut() {
                dialog.step = DialogStep::AgentOptions;
                dialog.field = 0;
            }
        }
        Step1Action::LaunchTerminal => launch_or_report(app),
    }
}

fn click_dir_entry(dialog: &mut NewAgentDialog, row_offset: usize) {
    let filtered = dialog.filtered_dir_entries();
    if filtered.is_empty() {
        return;
    }
    let window_scroll =
        crate::tui::selection::clamp_scroll(dialog.dir_selected, 0, filtered.len(), 10);
    let idx = window_scroll + row_offset;
    if idx >= filtered.len() {
        return;
    }
    dialog.field = 1;
    if dialog.dir_selected == idx {
        dialog.navigate_to_selected();
    } else {
        dialog.dir_selected = idx;
        dialog.update_dir_preview();
    }
}

fn handle_step2_click(app: &mut App, rel_x: usize, rel_y: usize) {
    enum Step2Action {
        None,
        Back,
        Next,
    }

    let action = {
        let Some(dialog) = app.new_agent_dialog.as_mut() else {
            return;
        };
        match rel_y {
            1 => {
                dialog.field = 0;
                if !dialog.available_clis.is_empty() {
                    let next = crate::tui::selection::move_index(
                        dialog.cli_index,
                        dialog.available_clis.len(),
                        true,
                    );
                    dialog.set_cli_index(next);
                }
                Step2Action::None
            }
            3 => {
                dialog.field = 1;
                if dialog.selected_yolo_flag().is_some() {
                    dialog.yolo_mode = !dialog.yolo_mode;
                }
                Step2Action::None
            }
            5 => {
                dialog.field = 2;
                dialog.sandbox_mode = !dialog.sandbox_mode;
                Step2Action::None
            }
            7 => {
                if rel_x < 16 {
                    Step2Action::Back
                } else {
                    Step2Action::Next
                }
            }
            _ => Step2Action::None,
        }
    };

    match action {
        Step2Action::None => {}
        Step2Action::Back => {
            if let Some(dialog) = app.new_agent_dialog.as_mut() {
                dialog.step = DialogStep::FolderAndKind;
                dialog.field = 0;
            }
        }
        Step2Action::Next => {
            let sessions = app
                .db
                .get_resumable_sessions(RESUME_CANDIDATE_FETCH_LIMIT)
                .unwrap_or_default();
            if let Some(dialog) = app.new_agent_dialog.as_mut() {
                mode_step::open_mode_step(dialog, sessions);
            }
        }
    }
}

fn handle_step3_click(app: &mut App, rel_y: usize, inner_height: usize) {
    let launch = {
        let Some(dialog) = app.new_agent_dialog.as_mut() else {
            return;
        };
        if rel_y == 1 {
            dialog.mode_index = NEW_ROW;
            dialog.task_mode = NewTaskMode::Interactive;
            true
        } else if rel_y == 2 {
            dialog.mode_index = RESUME_ROW;
            open_resume_list(dialog)
        } else if inner_height > 0 && rel_y + 1 == inner_height {
            if dialog.session_resume_picker.is_some() {
                dialog.session_resume_picker = None;
            } else if dialog.session_picker_open {
                dialog.session_picker_open = false;
            } else {
                dialog.step = DialogStep::AgentOptions;
                dialog.field = 0;
            }
            false
        } else if rel_y >= 4 {
            click_resume_list_row(dialog, rel_y - 4)
        } else {
            false
        }
    };

    if launch {
        launch_or_report(app);
    }
}

fn click_resume_list_row(dialog: &mut NewAgentDialog, row_offset: usize) -> bool {
    if let Some(picker) = dialog.session_resume_picker.as_mut() {
        let total = picker.total_rows();
        let scroll = crate::tui::selection::clamp_scroll(
            picker.index,
            0,
            total,
            SESSION_RESUME_PICKER_VISIBLE,
        );
        let idx = scroll + row_offset;
        if idx >= total {
            return false;
        }
        picker.index = idx;
        if picker.is_other_selected() {
            dialog.session_resume_picker = None;
            dialog.selected_resume_session = None;
            dialog.selected_session = None;
        } else if let Some(session) = picker.selected().cloned() {
            dialog.session_resume_picker = None;
            dialog.apply_resume_choice(session);
        }
        dialog.task_mode = NewTaskMode::Resume;
        return true;
    }

    if dialog.session_picker_open {
        let total = dialog.session_entries.len();
        let scroll = crate::tui::selection::clamp_scroll(dialog.session_picker_idx, 0, total, 6);
        let idx = scroll + row_offset;
        if idx >= total {
            return false;
        }
        dialog.session_picker_idx = idx;
        dialog.confirm_session_pick();
        dialog.task_mode = NewTaskMode::Resume;
        return true;
    }

    false
}

fn handle_scroll(app: &mut App, step: DialogStep, forward: bool) {
    let Some(dialog) = app.new_agent_dialog.as_mut() else {
        return;
    };
    match step {
        DialogStep::FolderAndKind => {
            let len = dialog.filtered_dir_entries().len();
            if len > 0 {
                dialog.dir_selected =
                    crate::tui::selection::move_index(dialog.dir_selected, len, forward);
                dialog.update_dir_preview();
            }
        }
        DialogStep::AgentOptions => {}
        DialogStep::Mode => {
            if let Some(picker) = dialog.session_resume_picker.as_mut() {
                picker.move_selection(forward, SESSION_RESUME_PICKER_VISIBLE);
            } else if dialog.session_picker_open && !dialog.session_entries.is_empty() {
                dialog.session_picker_idx = crate::tui::selection::move_index(
                    dialog.session_picker_idx,
                    dialog.session_entries.len(),
                    forward,
                );
            }
        }
    }
}

fn launch_or_report(app: &mut App) {
    if let Err(e) = app.launch_new_agent() {
        report_handler_error(app, "Could not open session", &e);
        app.close_new_agent_dialog();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::dialog::NewAgentDialog;

    fn app_with_rect(dialog: NewAgentDialog) -> App {
        let path = crate::test_support::temp_db_path();
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.new_agent_dialog = Some(dialog);
        app.last_new_agent_inner_rect = Some(Rect::new(10, 5, 60, 20));
        app
    }

    fn left_click(col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_navigates_step1_to_step2_to_step3_and_back() {
        let mut d = NewAgentDialog::new(Some("/tmp/proj"));
        d.available_clis = vec![
            crate::domain::models::Cli::new("claude"),
            crate::domain::models::Cli::new("codex"),
        ];
        d.cli_configs = vec![None, None];
        let mut app = app_with_rect(d);

        // Step 1: click `[ Next → ]` on row 1 (col >= 32 -> 10 + 35 = 45, row 5 + 1 = 6)
        handle_new_agent_dialog_mouse(&mut app, &left_click(45, 6));
        assert_eq!(
            app.new_agent_dialog.as_ref().unwrap().step,
            DialogStep::AgentOptions
        );

        // Step 2: click Sandbox on row 5 (5 + 5 = 10)
        handle_new_agent_dialog_mouse(&mut app, &left_click(15, 10));
        assert!(app.new_agent_dialog.as_ref().unwrap().sandbox_mode);

        // Step 2: click `[ Next → ]` on row 7 (col 10 + 25 = 35, row 5 + 7 = 12)
        handle_new_agent_dialog_mouse(&mut app, &left_click(35, 12));
        assert_eq!(
            app.new_agent_dialog.as_ref().unwrap().step,
            DialogStep::Mode
        );

        // Step 3: click footer to go back to Step 2 (row 5 + 19 = 24)
        handle_new_agent_dialog_mouse(&mut app, &left_click(15, 24));
        assert_eq!(
            app.new_agent_dialog.as_ref().unwrap().step,
            DialogStep::AgentOptions
        );

        // Step 2: click `[ ← Back ]` on row 7 (col 10 + 4 = 14, row 5 + 7 = 12)
        handle_new_agent_dialog_mouse(&mut app, &left_click(14, 12));
        assert_eq!(
            app.new_agent_dialog.as_ref().unwrap().step,
            DialogStep::FolderAndKind
        );
    }
}
