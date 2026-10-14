//! Launching a `send_to_session` background agent from the TUI dialog (CM53).
//!
//! The dialog's target picker lists the same live interactive sessions the
//! scheduled-send prompt uses. The created agent enqueues its prompt into the
//! chosen session on every fire instead of spawning a CLI.

use anyhow::Result;

use super::{agent_log_path, new_short_id};
use crate::application::ports::AgentRepository;
use crate::domain::models::{AgentAction, Trigger};
use crate::tui::app::dialog::new_agent::{BackgroundTrigger, NewAgentDialog};
use crate::tui::app::types::App;

impl App {
    /// Create a `send_to_session` agent from the dialog. Refuses (creating no
    /// agent) when the prompt is empty or no live session target is selected.
    pub(super) fn launch_send_to_session(&mut self, dialog: &NewAgentDialog) -> Result<()> {
        if dialog.prompt.is_empty() {
            return Ok(());
        }
        let Some(target) = dialog.selected_send_target() else {
            return Ok(());
        };
        let action =
            AgentAction::send_to_session(Some(target), None).map_err(anyhow::Error::msg)?;

        // Same guards `launch_watcher` applies: a watch agent with no path or
        // no parseable events would never fire.
        if matches!(dialog.background_trigger, BackgroundTrigger::Watch)
            && (dialog.watch_path.is_empty() || dialog.watch_events.is_empty())
        {
            return Ok(());
        }

        let id = new_short_id("agent");
        let working_dir = (!dialog.working_dir.is_empty()).then(|| dialog.working_dir.clone());
        let log_path = agent_log_path(&id);
        let mut agent = crate::daemon::handler_send_session::new_send_agent(
            id,
            dialog.prompt.clone(),
            action,
            working_dir,
            log_path,
        );
        agent.trigger = match dialog.background_trigger {
            BackgroundTrigger::Cron => Some(Trigger::Cron {
                schedule_expr: dialog.cron_expr.clone(),
            }),
            BackgroundTrigger::Watch => {
                let events: Vec<_> = dialog
                    .watch_events
                    .iter()
                    .filter_map(|e| crate::domain::models::WatchEvent::from_str(e))
                    .collect();
                Some(Trigger::Watch {
                    path: dialog.watch_path.clone(),
                    events,
                    debounce_seconds: 5,
                    recursive: false,
                })
            }
        };

        self.db.upsert_agent(&agent)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::dialog::new_agent::NewAgentDialog;
    use std::sync::Arc;
    use tempfile::tempdir;

    #[test]
    fn no_target_creates_no_agent() {
        let dir = tempdir().unwrap();
        let db = Arc::new(crate::db::Database::new(&dir.path().join("test.db")).unwrap());
        let data_dir = tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("app");

        let mut dialog = NewAgentDialog::new(None);
        dialog.task_type = crate::tui::app::dialog::new_agent::NewTaskType::Background;
        dialog.prompt = "hello".to_string();
        dialog.background_action =
            crate::tui::app::dialog::background_action::BackgroundAction::SendToSession;
        dialog.send_target_names = Vec::new();

        app.launch_send_to_session(&dialog).expect("launch");

        assert!(db.list_agents().unwrap().is_empty());
    }
}
