//! Inline rename prompt for the focused live interactive session.
//!
//! Pressing `r` on a focused `SidebarLayer::Live` `AgentEntry::Interactive`
//! card opens a short inline prompt (`Enter` submits via `session_rename`,
//! `Esc` cancels) wired to the same rename path as the MCP tool and CLI.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::types::{AgentEntry, App, SidebarLayer};

/// Open rename input for one live interactive session.
pub(crate) struct SessionRenameInput {
    pub session_id: String,
    pub input: String,
    pub error: Option<String>,
}

impl App {
    /// Open the rename prompt when `r` hits a focused Live interactive card.
    pub(crate) fn open_session_rename_if_focused(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> bool {
        if !matches!(code, KeyCode::Char('r'))
            || modifiers.contains(KeyModifiers::CONTROL)
            || modifiers.contains(KeyModifiers::ALT)
        {
            return false;
        }
        if self.session_rename_input.is_some() {
            return false;
        }
        if self.sidebar_layer != SidebarLayer::Live {
            return false;
        }
        let Some(AgentEntry::Interactive(idx)) = self.selected_agent() else {
            return false;
        };
        let Some(agent) = self.interactive_agents.get(*idx) else {
            return false;
        };
        self.session_rename_input = Some(SessionRenameInput {
            session_id: agent.id.clone(),
            input: agent.name.clone(),
            error: None,
        });
        true
    }

    /// Keys while the rename prompt is open; every key is consumed.
    pub(crate) fn session_rename_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(prompt) = self.session_rename_input.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => {
                self.session_rename_input = None;
            }
            KeyCode::Enter => self.submit_session_rename(),
            KeyCode::Backspace => {
                prompt.input.pop();
                prompt.error = None;
            }
            KeyCode::Char(c)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                prompt.input.push(c);
                prompt.error = None;
            }
            _ => {}
        }
        true
    }

    fn submit_session_rename(&mut self) {
        let Some(prompt) = self.session_rename_input.as_ref() else {
            return;
        };
        let target = prompt.session_id.clone();
        let new_name = prompt.input.trim().to_string();
        if let Err(err) = crate::domain::validation_session::validate_session_name(&new_name) {
            if let Some(prompt) = self.session_rename_input.as_mut() {
                prompt.error = Some(err);
            }
            return;
        }
        let port = self.daemon_port();
        let outcome = crate::tui::mcp_client::call_daemon_tool(
            &port,
            "session_rename",
            &serde_json::json!({ "target": target, "new_name": new_name }),
        );
        match outcome {
            Ok(outcome) if !outcome.is_error => {
                self.session_rename_input = None;
                self.apply_local_session_name(&target, &new_name);
            }
            Ok(outcome) => {
                if let Some(prompt) = self.session_rename_input.as_mut() {
                    prompt.error = Some(outcome.text);
                }
            }
            Err(err) => {
                if let Some(prompt) = self.session_rename_input.as_mut() {
                    prompt.error = Some(format!("{err:#}"));
                }
            }
        }
    }

    fn apply_local_session_name(&mut self, id: &str, name: &str) {
        if let Some(agent) = self.interactive_agents.iter_mut().find(|a| a.id == id) {
            agent.name = name.to_string();
            self.reindex_sessions();
            self.slow_force = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_app() -> App {
        let path = crate::test_support::temp_db_path();
        let db = Arc::new(crate::db::Database::new(&path).expect("create test db"));
        let dir = tempfile::tempdir().expect("data dir");
        App::new(
            Arc::clone(&db),
            dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    fn spawn_live(app: &mut App, name: &str) {
        let agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some(name),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn mock interactive");
        app.interactive_agents.push(agent);
        app.agents = vec![AgentEntry::Interactive(0)];
        app.selected = 0;
        app.sidebar_layer = SidebarLayer::Live;
    }

    #[test]
    fn open_rename_on_live_interactive_card() {
        let mut app = test_app();
        spawn_live(&mut app, "alpha");
        assert!(app.open_session_rename_if_focused(KeyCode::Char('r'), KeyModifiers::NONE));
        let prompt = app.session_rename_input.as_ref().expect("prompt open");
        assert_eq!(prompt.input, "alpha");
    }

    #[test]
    fn rename_ignores_other_layers_and_keys() {
        let mut app = test_app();
        spawn_live(&mut app, "alpha");
        assert!(!app.open_session_rename_if_focused(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.session_rename_input.is_none());
        app.sidebar_layer = SidebarLayer::Automation;
        assert!(!app.open_session_rename_if_focused(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(app.session_rename_input.is_none());
        app.sidebar_layer = SidebarLayer::Live;
        assert!(!app.open_session_rename_if_focused(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(app.session_rename_input.is_none());
    }

    #[test]
    fn esc_cancels_and_typing_edits() {
        let mut app = test_app();
        spawn_live(&mut app, "alpha");
        assert!(app.open_session_rename_if_focused(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(app.session_rename_key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.session_rename_key(KeyCode::Backspace, KeyModifiers::NONE));
        let prompt = app.session_rename_input.as_ref().expect("still open");
        assert_eq!(prompt.input, "alpha");
        assert!(app.session_rename_key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.session_rename_input.is_none());
    }

    #[test]
    fn enter_with_invalid_name_shows_error() {
        let mut app = test_app();
        spawn_live(&mut app, "alpha");
        assert!(app.open_session_rename_if_focused(KeyCode::Char('r'), KeyModifiers::NONE));
        if let Some(prompt) = app.session_rename_input.as_mut() {
            prompt.input = "BAD NAME".to_string();
        }
        assert!(app.session_rename_key(KeyCode::Enter, KeyModifiers::NONE));
        let prompt = app.session_rename_input.as_ref().expect("kept open");
        assert!(prompt.error.is_some(), "validation error shown");
    }
}
