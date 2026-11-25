use anyhow::Result;
use ratatui::crossterm::event::KeyCode;

use crate::tui::app::types::App;
use crate::tui::context_transfer::ContextTransferStep;

// ── Context Transfer modal ───────────────────────────────────────
//
// Step 1 (Preview):  ↑↓ select part, Space toggle, Enter → Step 2, Esc → cancel.
// Step 2 (Picker):   ↑↓ navigate agents, Enter → execute, Esc → back.

/// Rebuild the payload_preview string from the current source agent state.
pub fn ctx_rebuild_preview(app: &mut App) {
    app.refresh_context_transfer_preview();
}

pub fn handle_context_transfer_key(app: &mut App, code: KeyCode) -> Result<()> {
    let Some(modal) = app.context_transfer_modal.as_ref() else {
        app.focus = crate::tui::app::types::Focus::Agent;
        return Ok(());
    };

    match modal.step {
        ContextTransferStep::Preview => match code {
            KeyCode::Esc => {
                app.close_context_transfer_modal();
            }
            KeyCode::Enter => {
                app.context_transfer_to_picker();
            }
            KeyCode::Up | KeyCode::Down => {
                let forward = matches!(code, KeyCode::Down);
                if let Some(modal) = app.context_transfer_modal.as_mut() {
                    modal.selected_part =
                        crate::tui::selection::move_index(modal.selected_part, 3, forward);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(modal) = app.context_transfer_modal.as_mut() {
                    modal.toggle_selected_part();
                }
                ctx_rebuild_preview(app);
            }
            _ => {}
        },
        ContextTransferStep::AgentPicker => match code {
            KeyCode::Esc => {
                // Go back to preview step
                if let Some(modal) = app.context_transfer_modal.as_mut() {
                    modal.step = ContextTransferStep::Preview;
                }
            }
            KeyCode::Up | KeyCode::Down => {
                let picker_len = app.picker_interactive_entries().len();
                if picker_len == 0 {
                    return Ok(());
                }
                let forward = matches!(code, KeyCode::Down);
                if let Some(modal) = app.context_transfer_modal.as_mut() {
                    modal.picker_selected = crate::tui::selection::move_index(
                        modal.picker_selected,
                        picker_len,
                        forward,
                    );
                }
            }
            KeyCode::Enter => {
                let dest_idx = app
                    .context_transfer_modal
                    .as_ref()
                    .map(|m| m.picker_selected)
                    .unwrap_or(0);
                app.execute_context_transfer(dest_idx);
            }
            _ => {}
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::handle_context_transfer_key;
    use crate::db::Database;
    use crate::tui::agent::InteractiveAgent;
    use crate::tui::app::types::{AgentEntry, App};
    use ratatui::crossterm::event::KeyCode;
    use ratatui::style::Color;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn app_with_modal() -> App {
        let db = Arc::new(Database::new(&crate::test_support::temp_db_path()).expect("db"));
        let data_dir = tempdir().expect("data dir");
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("app");
        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            10,
            Some("src-term"),
            &[],
            Color::White,
        )
        .expect("spawn");
        {
            let mut vt = agent.vt.lock().unwrap();
            vt.process(b"screen-line-one\r\n");
        }
        agent.record_prompt("last user prompt");
        app.terminal_agents.push(agent);
        app.agents = vec![AgentEntry::Terminal(0)];
        app.selected = 0;
        app.open_context_transfer_modal();
        app
    }

    #[test]
    fn preview_navigation_and_space_toggle_updates_payload_preview() {
        let mut app = app_with_modal();

        let initial = app
            .context_transfer_modal
            .as_ref()
            .unwrap()
            .payload_preview
            .clone();
        assert!(initial.contains("--- context from terminal: src-term"));
        assert!(initial.contains("screen-line-one"));
        assert!(initial.contains("> last user prompt"));

        // Toggle part 0 (Header) off
        handle_context_transfer_key(&mut app, KeyCode::Char(' ')).unwrap();
        let after_header_off = app.context_transfer_modal.as_ref().unwrap();
        assert!(!after_header_off.parts.include_header);
        assert!(!after_header_off
            .payload_preview
            .contains("--- context from"));
        assert!(after_header_off.payload_preview.contains("screen-line-one"));

        // Move Down to part 1 (Screen tail) and toggle off
        handle_context_transfer_key(&mut app, KeyCode::Down).unwrap();
        assert_eq!(
            app.context_transfer_modal.as_ref().unwrap().selected_part,
            1
        );
        handle_context_transfer_key(&mut app, KeyCode::Char(' ')).unwrap();
        let after_screen_off = app.context_transfer_modal.as_ref().unwrap();
        assert!(!after_screen_off.parts.include_screen);
        assert!(!after_screen_off.payload_preview.contains("screen-line-one"));
        assert!(after_screen_off
            .payload_preview
            .contains("> last user prompt"));

        // Move Down to part 2 (Last prompt) and toggle off -> empty payload
        handle_context_transfer_key(&mut app, KeyCode::Down).unwrap();
        assert_eq!(
            app.context_transfer_modal.as_ref().unwrap().selected_part,
            2
        );
        handle_context_transfer_key(&mut app, KeyCode::Char(' ')).unwrap();
        let after_all_off = app.context_transfer_modal.as_ref().unwrap();
        assert!(!after_all_off.parts.include_last_prompt);
        assert!(after_all_off.payload_preview.is_empty());

        // Wrap around with Down -> 0, Up -> 2
        handle_context_transfer_key(&mut app, KeyCode::Down).unwrap();
        assert_eq!(
            app.context_transfer_modal.as_ref().unwrap().selected_part,
            0
        );
        handle_context_transfer_key(&mut app, KeyCode::Up).unwrap();
        assert_eq!(
            app.context_transfer_modal.as_ref().unwrap().selected_part,
            2
        );
    }
}
