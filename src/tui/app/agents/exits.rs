use super::{log_terminal_exit, recent_output_snippet, SessionTarget};
use crate::domain::nursery;
use crate::domain::sandbox;
use crate::tui::agent::AgentStatus;
use crate::tui::app::types::App;
use crate::tui::gamification::MissionEvent;
use crate::tui::whimsg;

impl App {
    pub(super) fn exited_terminal_indices(&self) -> Vec<usize> {
        self.terminal_agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| matches!(agent.status, AgentStatus::Exited(_)))
            .map(|(idx, _)| idx)
            .collect()
    }

    pub(super) fn handle_terminal_exit(&mut self, idx: usize) {
        let Some(agent) = self.terminal_agents.get(idx) else {
            return;
        };
        if agent.exit_notified {
            return;
        }

        let AgentStatus::Exited(code) = agent.status else {
            return;
        };

        let agent_id = agent.id.clone();
        let agent_name = agent.name.clone();
        let shell = agent.shell.clone();
        let output_snippet = recent_output_snippet(agent, 5);

        let _ = self.db.finish_terminal_session(&agent_id);
        if code != 0 {
            log_terminal_exit(&agent_name, &shell, code, &output_snippet);
        }

        let Some(agent) = self.terminal_agents.get_mut(idx) else {
            return;
        };
        agent.exit_notified = true;
    }

    pub(super) fn exited_interactive_indices(&self) -> Vec<usize> {
        self.interactive_agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| matches!(agent.status, AgentStatus::Exited(_)))
            .map(|(idx, _)| idx)
            .collect()
    }

    pub(super) fn notify_failed_interactive_exit(
        &mut self,
        agent_id: &str,
        cli: &str,
        code: i32,
        output_snippet: &str,
    ) {
        tracing::warn!(
            "Agent '{agent_id}' ({cli}) exited with code {code}.{}",
            if output_snippet.is_empty() {
                ""
            } else {
                output_snippet
            }
        );

        let subject = self
            .interactive_agents
            .iter()
            .find(|agent| agent.id == agent_id)
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| agent_id.to_string());
        self.whimsg.notify_event(whimsg::events::WhimEvent::new(
            whimsg::events::EventKind::AgentFailed,
            subject,
        ));
        if !self.notifications_enabled {
            return;
        }

        let output = output_snippet.trim_start_matches('\n').to_string();
        self.notification_service
            .notify_agent_failed(agent_id, cli, code, &output);
    }

    pub(super) fn handle_interactive_exit(&mut self, idx: usize) {
        let Some(agent) = self.interactive_agents.get(idx) else {
            return;
        };
        if agent.exit_notified {
            return;
        }

        let AgentStatus::Exited(code) = agent.status else {
            return;
        };

        let agent_id = agent.id.clone();
        let agent_name = agent.name.clone();
        let working_dir = agent.working_dir.clone();
        let cli = agent.cli.as_str().to_string();
        let output_snippet = recent_output_snippet(agent, 5);
        self.record_interactive_exit_in_db(&agent_id, &working_dir, code);
        self.merge_active_sandbox();
        let _ = self
            .db
            .close_agent_missions(&agent_id, &agent_name, &working_dir);
        self.announce_interactive_exit(&agent_id, &cli, code, &output_snippet);
        if let Some(agent) = self.interactive_agents.get_mut(idx) {
            agent.exit_notified = true;
        }
    }

    pub(super) fn record_interactive_exit_in_db(
        &mut self,
        agent_id: &str,
        working_dir: &str,
        code: i32,
    ) {
        let Some(nursery_path) = self.nursery_path.clone() else {
            let _ = self.db.finish_interactive_session(agent_id, code); // non-nursery path
            return;
        };
        if nursery_path.to_string_lossy() == working_dir && code == 0 {
            match nursery::finalize_nursery(&nursery_path) {
                Ok(seed_id) => {
                    let _ = self.db.bind_session_to_seed(agent_id, &seed_id);
                    self.queue_mission_event(MissionEvent::FirstSeedCreated);
                }
                Err(e) => {
                    tracing::error!("Nursery finalization failed: {e}");
                    self.notification_service.notify_nursery_failed(&e);
                }
            }
        } else if code != 0 {
            let _ = std::fs::remove_dir_all(&nursery_path);
        }
        let _ = self.db.remove_interactive_session(agent_id);
        let _ = self.db.unregister_project_path(&nursery_path);
        self.nursery_path = None;
    }

    pub(super) fn merge_active_sandbox(&mut self) {
        let Some(sandbox) = self.active_sandbox.clone() else {
            return;
        };
        let merge_result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { crate::domain::sandbox::merge_sandbox(&sandbox).await })
        });
        match merge_result {
            Ok(outcome) => {
                let _ = self.db.update_sandbox_run_status(
                    &sandbox.id,
                    Self::sandbox_merge_run_status(&outcome),
                );
                if let crate::domain::sandbox::MergeOutcome::MergeFailed(ref reason) = outcome {
                    tracing::error!("Sandbox merge failed: {reason}");
                    self.notification_service
                        .notify_nursery_failed(&format!("Sandbox merge failed: {reason}"));
                }
            }
            Err(e) => {
                tracing::error!("Sandbox merge error: {e}");
                let _ = self.db.update_sandbox_run_status(&sandbox.id, "failed");
            }
        }
        self.active_sandbox = None;
    }

    pub(super) fn sandbox_merge_run_status(
        outcome: &crate::domain::sandbox::MergeOutcome,
    ) -> &'static str {
        match outcome {
            sandbox::MergeOutcome::CleanMerge => "merged",
            sandbox::MergeOutcome::ConflictResolution => "merged",
            sandbox::MergeOutcome::MergeFailed(_) => "failed",
        }
    }

    pub(super) fn announce_interactive_exit(
        &mut self,
        agent_id: &str,
        cli: &str,
        code: i32,
        output_snippet: &str,
    ) {
        if code != 0 {
            self.notify_failed_interactive_exit(agent_id, cli, code, output_snippet);
            return;
        }
        if self.session_args_contain_yolo(agent_id, cli) {
            self.queue_mission_event(MissionEvent::YoloTaskCompleted);
        }
    }

    pub(super) fn successful_interactive_exit_indices(&self) -> Vec<usize> {
        // Keep a successfully-finished session on screen while it is the one the
        // user is currently viewing — its final output often carries useful
        // end-of-run stats. It is reaped on a later poll, once the user moves
        // the selection to another session.
        let viewing = match self.selected_session_target() {
            Some(SessionTarget::Interactive(idx)) => Some(idx),
            _ => None,
        };
        self.interactive_agents
            .iter()
            .enumerate()
            .filter(|(idx, agent)| {
                matches!(agent.status, AgentStatus::Exited(0)) && Some(*idx) != viewing
            })
            .map(|(idx, _)| idx)
            .collect()
    }
}
