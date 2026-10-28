use anyhow::Result;
use std::collections::HashSet;

use crate::application::ports::{AgentRepository, RunRepository, StateRepository};

use super::types::{AgentEntry, App};
use super::utils::{is_local_port_open, is_process_running, relative_time, tail_lines};

impl App {
    pub(super) fn refresh_daemon_status(&mut self) {
        let pid_path = self.data_dir.join("daemon.pid");
        self.daemon_pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|s| s.trim().parse().ok());
        let daemon_running_by_pid = self.daemon_pid.map(is_process_running).unwrap_or(false);
        self.daemon_version = self
            .db
            .get_state("version")
            .ok()
            .flatten()
            .unwrap_or_default();
        let daemon_port = self
            .db
            .get_state("port")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(7755);
        let daemon_running_by_port = is_local_port_open(daemon_port);
        self.daemon_running = daemon_running_by_pid || daemon_running_by_port;
    }

    pub(super) fn refresh_agents(&mut self) -> Result<()> {
        let agents = self.db.list_agents()?;
        // Corrupt rows (e.g. malformed trigger_config) never fail this
        // refresh — they're shown as degraded cards instead.
        let corrupt = self.db.list_corrupt_agents().unwrap_or_default();

        self.agents.clear();
        // Background agents first (they are rendered at the top of the sidebar)
        for a in agents {
            self.agents.push(AgentEntry::Agent(a));
        }
        for c in corrupt {
            self.agents.push(AgentEntry::Corrupt(c));
        }
        // Interactive sessions
        for i in 0..self.interactive_agents.len() {
            self.agents.push(AgentEntry::Interactive(i));
        }
        // Then terminals
        for i in 0..self.terminal_agents.len() {
            self.agents.push(AgentEntry::Terminal(i));
        }
        // Orphaned sessions (can be revived or dismissed)
        for i in 0..self.orphaned_sessions.len() {
            self.agents.push(AgentEntry::Orphaned(i));
        }
        // Then split groups
        for i in 0..self.split_groups.len() {
            self.agents.push(AgentEntry::Group(i));
        }

        let total = self.agents.len();
        if total > 0 && self.selected >= total {
            self.selected = total - 1;
        }

        Ok(())
    }

    pub(super) fn refresh_active_runs(&mut self) -> Result<()> {
        let prev_ids = std::mem::take(&mut self.prev_active_run_ids);

        self.active_runs.clear();
        for agent in &self.agents {
            let id = agent.id(self);
            if let Ok(Some(run)) = self.db.get_active_run(id) {
                self.active_runs.insert(id.to_string(), run);
            }
        }

        // Detect background task completions: was active last tick, gone now.
        //
        // The daemon's executor (`notify_result`) is the single owner of
        // desktop notifications for background-agent runs — when it's alive it
        // fires the completion toast, so the TUI must stay silent to avoid a
        // duplicate toast for the same run. This TUI-side sender exists only as
        // a fallback for a daemonless TUI (no daemon process detected), so it
        // still surfaces completions when nothing else would.
        if self.notifications_enabled && !self.daemon_running {
            self.notify_finished_runs(&prev_ids);
        }
        self.prev_active_run_ids = self.active_runs.keys().cloned().collect();

        self.recent_runs = self.db.list_all_recent_runs(50)?;
        Ok(())
    }

    fn notify_finished_runs(&mut self, prev_ids: &HashSet<String>) {
        if !self.notifications_enabled || self.daemon_running {
            return;
        }
        for finished_id in prev_ids {
            self.notify_finished_run(finished_id);
        }
    }

    fn notify_finished_run(&mut self, finished_id: &str) {
        if self.active_runs.contains_key(finished_id) {
            return;
        }
        if self.db.get_agent(finished_id).ok().flatten().is_none() {
            return;
        }
        let Some(run) = self
            .db
            .list_runs(finished_id, 1)
            .ok()
            .and_then(|mut runs| runs.drain(..).next())
        else {
            return;
        };
        let success = matches!(run.status, crate::domain::models::RunStatus::Success);
        self.notification_service
            .notify_task_completed(finished_id, success, run.exit_code);
    }

    pub(super) fn refresh_log(&mut self) {
        let Some(agent) = self.agents.get(self.selected) else {
            self.log_content = String::new();
            return;
        };

        match agent {
            AgentEntry::Interactive(idx) => {
                if *idx >= self.interactive_agents.len() {
                    self.log_content = String::from("Agent removed");
                    return;
                }
                let output = self.interactive_agents[*idx].output();
                self.log_content = if output.is_empty() {
                    format!(
                        "Agent '{}' — waiting for output...",
                        self.interactive_agents[*idx].id
                    )
                } else {
                    output
                };
            }
            AgentEntry::Terminal(idx) => {
                if *idx >= self.terminal_agents.len() {
                    self.log_content = String::from("Terminal removed");
                    return;
                }
                let output = self.terminal_agents[*idx].output();
                self.log_content = if output.is_empty() {
                    format!(
                        "Terminal '{}' — waiting for output...",
                        self.terminal_agents[*idx].name
                    )
                } else {
                    output
                };
            }
            AgentEntry::Group(idx) => {
                if let Some(group) = self.split_groups.get(*idx) {
                    self.log_content = format!(
                        "Split Group: {}\n{} · {}\nOrientation: {}",
                        group.id,
                        group.session_a,
                        group.session_b,
                        group.orientation.as_str(),
                    );
                }
            }
            _ => {
                let id = agent.id(self).to_string();
                let log_path = self.data_dir.join("logs").join(format!("{id}.log"));

                let mut content = match std::fs::read_to_string(&log_path) {
                    Ok(c) => tail_lines(&c, 200),
                    Err(_) => String::new(),
                };

                if let Some(run) = self.active_runs.get(&id) {
                    let header = format!(
                        "⏳ Run {} in progress ({})\n{}\n",
                        &run.id[..8.min(run.id.len())],
                        relative_time(&run.started_at),
                        "─".repeat(40),
                    );
                    content = if content.is_empty() {
                        format!("{header}Waiting for output...")
                    } else {
                        format!("{header}{content}")
                    };
                } else if content.is_empty() {
                    content = format!("No logs yet for '{id}'");
                }

                self.log_content = content;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::application::ports::AgentRepository;
    use crate::db::Database;
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

    // ── notify_finished_run tests ───────────────────────────────────────
    // Three early-return branches must be covered:
    // 1. run still in active_runs (should not notify)
    // 2. agent not found in DB (should not notify)
    // 3. no runs found for agent (should not notify)

    #[test]
    fn notify_finished_run_skips_when_still_active() {
        let (mut app, _dir) = test_app();
        app.notifications_enabled = true;
        app.daemon_running = false;

        // Add to active_runs - should skip notification
        app.active_runs.insert(
            "agent1".to_string(),
            crate::domain::models::RunLog {
                id: "run1".into(),
                background_agent_id: "agent1".into(),
                status: crate::domain::models::RunStatus::InProgress,
                trigger_type: crate::domain::models::TriggerType::Manual,
                summary: None,
                started_at: chrono::Utc::now(),
                finished_at: None,
                exit_code: None,
                timeout_at: None,
                executed_platform: None,
                executed_model: None,
            },
        );

        // This should not panic and should return early
        app.notify_finished_run("agent1");
    }

    #[test]
    fn notify_finished_run_skips_when_agent_not_found() {
        let (mut app, _dir) = test_app();
        app.notifications_enabled = true;
        app.daemon_running = false;

        // Agent "nonexistent" is not in the DB - should skip
        app.notify_finished_run("nonexistent");
    }

    #[test]
    fn notify_finished_run_skips_when_no_runs() {
        let (mut app, _dir) = test_app();
        app.notifications_enabled = true;
        app.daemon_running = false;

        // Create an agent in the DB but with no runs
        use crate::domain::models::{Agent, Cli, Trigger};
        use chrono::Utc;
        let agent = Agent {
            id: "agent-no-runs".into(),
            prompt: "test".into(),
            trigger: Some(Trigger::Cron {
                schedule_expr: "0 9 * * *".into(),
            }),
            cli: Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: "/tmp/test.log".into(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        };
        app.db.upsert_agent(&agent).expect("insert agent");

        // Should skip because no runs exist for this agent
        app.notify_finished_run("agent-no-runs");
    }
}
