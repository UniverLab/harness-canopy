//! Agent launch and edit-persistence: creating/deleting agents from the
//! new-agent and launchpad dialogs, and applying edits back to the DB.
//! Split out of `app_methods.rs` to keep every production file within the
//! file-size gate.

use std::path::Path;

use anyhow::Result;

use super::{
    agent_log_path, apply_scheduled_edit, apply_watcher_edit, new_short_id, pty_dimensions,
};
use crate::application::ports::AgentRepository;
use crate::tui::app::dialog::launchpad::{LaunchpadChoice, LaunchpadDialog};
use crate::tui::app::dialog::new_agent::{
    BackgroundTrigger, NewAgentDialog, NewTaskMode, NewTaskType,
};
use crate::tui::app::types::App;

impl App {
    pub fn launch_new_agent(&mut self) -> Result<()> {
        // Take dialog out of self to avoid borrow conflicts
        let Some(dialog) = self.new_agent_dialog.take() else {
            return Ok(());
        };

        let model = if dialog.model.is_empty() {
            None
        } else {
            Some(dialog.model.clone())
        };

        let _was_interactive = matches!(
            dialog.task_type,
            NewTaskType::Interactive | NewTaskType::Terminal
        );
        let prev_focus = dialog.prev_focus;

        if let Some(ref edit_id) = dialog.edit_id {
            // ── Edit mode: partial-update existing agent ──────────────────
            let model_ref = model.as_deref();
            match dialog.task_type {
                NewTaskType::Background => match dialog.background_trigger {
                    BackgroundTrigger::Cron => {
                        self.update_scheduled(&dialog, model_ref, edit_id)?;
                    }
                    BackgroundTrigger::Watch => {
                        self.update_watcher_edit(&dialog, model_ref, edit_id)?;
                    }
                },
                NewTaskType::Interactive | NewTaskType::Terminal => {}
            }
            self.new_agent_dialog = None;
            self.refresh_agents()?;
            self.focus = prev_focus.unwrap_or(crate::tui::app::types::Focus::Preview);
            return Ok(());
        }

        // ── Create mode ───────────────────────────────────────────────────
        if matches!(dialog.task_type, NewTaskType::Interactive) {
            // New (the mission step) is the launchpad; resuming and nursery
            // sessions go straight to the PTY as they always have (CT34).
            if dialog.is_planting_new_seed() || matches!(dialog.task_mode, NewTaskMode::Resume) {
                self.launch_interactive(&dialog)?;
                let name = self.take_new_interactive_name();
                self.new_agent_dialog = None;
                self.refresh_agents()?;
                self.select_agent_by_name(&name);
                self.focus = crate::tui::app::types::Focus::Agent;
                return Ok(());
            } else {
                self.open_launchpad_dialog(dialog)?;
                return Ok(());
            }
        }

        // Track the name of the newly created agent to select it after refresh
        let new_agent_name = self.launch_created_agent(&dialog, model)?;

        self.new_agent_dialog = None;

        self.refresh_agents()?;

        // Select the newly created agent specifically instead of just the last agent
        if let Some(name) = new_agent_name {
            self.select_agent_by_name(&name);
        }

        // All new sessions start in focus mode
        self.focus = crate::tui::app::types::Focus::Agent;
        Ok(())
    }

    pub(super) fn select_agent_by_name(&mut self, agent_name: &str) {
        if agent_name.is_empty() {
            return;
        }
        if let Some(position) = self
            .agents
            .iter()
            .position(|entry| entry.id(self) == agent_name)
        {
            self.selected = position;
        }
    }

    fn take_new_interactive_name(&self) -> String {
        self.interactive_agents
            .last()
            .map(|agent| agent.name.clone())
            .unwrap_or_default()
    }

    pub(super) fn launch_created_agent(
        &mut self,
        dialog: &NewAgentDialog,
        model: Option<String>,
    ) -> Result<Option<String>> {
        match dialog.task_type {
            NewTaskType::Background => {
                if dialog.is_send_to_session() {
                    self.launch_send_to_session(dialog)?;
                    return Ok(None);
                }
                match dialog.background_trigger {
                    BackgroundTrigger::Cron => {
                        self.launch_scheduled(dialog, model)?;
                    }
                    BackgroundTrigger::Watch => {
                        self.launch_watcher(dialog, model)?;
                    }
                }
                Ok(None)
            }
            NewTaskType::Terminal => {
                self.launch_terminal(dialog)?;
                Ok(self.terminal_agents.last().map(|agent| agent.name.clone()))
            }
            NewTaskType::Interactive => Ok(None), // handled above
        }
    }

    pub(super) fn open_launchpad_dialog(&mut self, dialog: NewAgentDialog) -> Result<()> {
        let launchpad = LaunchpadDialog::for_workdir(&self.db, &dialog.working_dir)?;
        self.pending_launch_dialog = Some(dialog);
        self.launchpad_dialog = Some(launchpad);
        self.focus = crate::tui::app::types::Focus::LaunchpadDialog;
        Ok(())
    }

    pub fn confirm_launchpad_dialog(&mut self) -> Result<()> {
        let can_confirm = self
            .launchpad_dialog
            .as_ref()
            .is_some_and(LaunchpadDialog::can_confirm_selection);
        if !can_confirm {
            return Ok(());
        }

        let Some(dialog) = self.pending_launch_dialog.take() else {
            self.launchpad_dialog = None;
            self.focus = crate::tui::app::types::Focus::Preview;
            return Ok(());
        };
        let Some(launchpad) = self.launchpad_dialog.take() else {
            self.focus = crate::tui::app::types::Focus::Preview;
            return Ok(());
        };

        let Some((mission_title, mission_context, _previous_node_id, mode)) =
            Self::launchpad_mission_choice(&launchpad)
        else {
            return Ok(());
        };
        // `continues` edge to previous node is dropped — operational
        // sessions have no edges (CM8, out of scope schema).

        let _launchpad_node_id = Self::record_launchpad_session(
            self,
            &mission_title,
            mission_context.as_deref(),
            mode,
            &dialog.working_dir,
        )?;

        let is_nursery = dialog.is_planting_new_seed();
        self.launch_interactive(&dialog)?;
        let name = self.take_new_interactive_name();
        self.refresh_agents()?;
        self.select_agent_by_name(&name);

        if is_nursery {
            self.focus = crate::tui::app::types::Focus::Agent;
            return Ok(());
        }

        let initial_content = Self::launchpad_initial_content(
            self,
            &dialog.working_dir,
            &mission_title,
            mission_context.as_deref(),
            &launchpad,
        );
        self.focus = crate::tui::app::types::Focus::Agent;
        if !dialog.is_planting_new_seed() {
            self.open_simple_prompt_dialog(Some(initial_content));
        }
        Ok(())
    }

    pub(super) fn launchpad_mission_choice(
        launchpad: &LaunchpadDialog,
    ) -> Option<(String, Option<String>, Option<String>, &'static str)> {
        match launchpad.choice() {
            LaunchpadChoice::ContinueMission => {
                let previous = launchpad.selected_mission()?;
                Some((
                    previous.mission.clone(),
                    previous.summary.clone(),
                    Some(previous.node_id.clone()),
                    "continue",
                ))
            }
            LaunchpadChoice::NewMission => {
                let mission = launchpad.new_mission_title()?;
                Some((mission.to_string(), None, None, "new"))
            }
        }
    }

    fn record_launchpad_session(
        &self,
        title: &str,
        mission_context: Option<&str>,
        mode: &str,
        workdir: &str,
    ) -> Result<String> {
        let launchpad_node_id = format!("launchpad:{}", uuid::Uuid::new_v4());
        let body = mission_context
            .map(str::to_string)
            .unwrap_or_else(|| "Launchpad session started.".to_string());
        self.db
            .upsert_operational_session(crate::db::intelligence::OperationalSessionInput {
                id: Some(launchpad_node_id.clone()),
                title: title.to_string(),
                body,
                metadata: Some(serde_json::json!({
                    "source": "launchpad",
                    "workdir": workdir,
                    "mode": mode,
                    "summary": mission_context,
                })),
                project_hash: None,
                session_id: Some(launchpad_node_id.clone()),
            })?;
        Ok(launchpad_node_id)
    }

    pub(super) fn launchpad_initial_content(
        &self,
        workdir: &str,
        mission_title: &str,
        mission_context: Option<&str>,
        launchpad: &LaunchpadDialog,
    ) -> std::collections::HashMap<String, String> {
        let mut initial_content = std::collections::HashMap::new();
        let mut launchpad_context = format!("mission: {mission_title}");
        if let Some(context) = mission_context {
            if !context.trim().is_empty() {
                launchpad_context.push_str("\n\nprevious_summary:\n");
                launchpad_context.push_str(context.trim());
            }
        }
        if !launchpad.active_missions.is_empty() {
            launchpad_context.push_str("\n\nactive_peer_missions:\n");
            for m in &launchpad.active_missions {
                launchpad_context.push_str(&format!(
                    "- {} [{}]: {}\n",
                    m.agent_name, m.impact, m.mission
                ));
            }
        }
        initial_content.insert("context".to_string(), launchpad_context);
        if let Ok(Some(project)) = self.db.get_project_by_path_or_ancestor(Path::new(workdir)) {
            initial_content.insert("project_context".to_string(), project.path);
        }
        initial_content
    }

    pub(super) fn update_scheduled(
        &self,
        dialog: &NewAgentDialog,
        model: Option<&str>,
        id: &str,
    ) -> Result<()> {
        if dialog.prompt.is_empty() {
            return Ok(());
        }
        let Some(mut agent) = self.db.get_agent(id)? else {
            return Ok(());
        };
        apply_scheduled_edit(&mut agent, dialog, model);
        self.db.upsert_agent(&agent)?;
        Ok(())
    }

    pub(super) fn update_watcher_edit(
        &self,
        dialog: &NewAgentDialog,
        model: Option<&str>,
        id: &str,
    ) -> Result<()> {
        if dialog.prompt.is_empty() || dialog.watch_path.is_empty() {
            return Ok(());
        }
        let Some(mut agent) = self.db.get_agent(id)? else {
            return Ok(());
        };
        apply_watcher_edit(&mut agent, dialog, model);
        self.db.upsert_agent(&agent)?;
        Ok(())
    }

    pub(super) fn launch_interactive(&mut self, dialog: &NewAgentDialog) -> Result<()> {
        use crate::tui::agent::InteractiveAgent;
        let cli = dialog.selected_cli();
        self.record_cli_usage(cli.as_str());

        // Check if planting a new seed via Nursery
        let (dir, is_nursery) = if dialog.is_planting_new_seed() {
            let nursery_dir = crate::domain::nursery::create_nursery(cli.as_str(), None)
                .map_err(|e| anyhow::anyhow!(e))?;
            let d = nursery_dir.to_string_lossy().to_string();
            // Store nursery path for finalization on session end
            self.nursery_path = Some(nursery_dir);
            (d, true)
        } else if dialog.sandbox_mode {
            let protocol = crate::domain::sandbox::canopy_protocol_block();
            let workdir = dialog.working_dir.clone();
            let cli_name = cli.as_str().to_string();
            let sb = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    crate::domain::sandbox::create_sandbox(&workdir, &cli_name, protocol).await
                })
            })?;
            let agent_id_placeholder = format!("interactive-{}", &sb.id[..8]);
            let _ = self
                .db
                .insert_sandbox_run(&sb, "interactive", &agent_id_placeholder);
            self.active_sandbox = Some(sb.clone());
            (sb.worktree_path.to_string_lossy().to_string(), false)
        } else {
            (dialog.working_dir.clone(), false)
        };

        // The nursery instruction file is written only inside the ephemeral
        // nursery temp dir by `create_nursery`; normal sessions must never get
        // the Gardener instructions written into their working directory.

        let args = yolo_launch_args(dialog);
        let fallback = dialog.selected_fallback_args();
        let accent = dialog.selected_accent_color(&self.theme);
        let model = if dialog.model.is_empty() {
            None
        } else {
            Some(dialog.model.clone())
        };
        let model_flag = dialog
            .cli_configs
            .get(dialog.cli_index)
            .and_then(|c| c.as_ref())
            .and_then(|c| c.model_flag.clone());
        let (harness_conversation_id, known_session) = self.resolve_launch_identity(dialog);
        let (cols, rows) = pty_dimensions(self.last_panel_inner);
        let existing_refs: Vec<&str> = self
            .interactive_agents
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        // For nursery sessions, don't pass seed_id (it gets bound on finalization)
        let seed_id = if is_nursery {
            None
        } else {
            dialog.selected_seed_id()
        };
        let agent_name = if is_nursery { Some("Gardener") } else { None };
        let spawn_name = known_session
            .as_ref()
            .map(|session| session.name.clone())
            .or_else(|| agent_name.map(str::to_string));
        let spawn_id = known_session.as_ref().map(|session| session.id.clone());
        let agent = InteractiveAgent::spawn(
            cli,
            &dir,
            cols,
            rows,
            args.as_deref(),
            fallback.as_deref(),
            accent,
            spawn_name.as_deref(),
            spawn_id.as_deref(),
            &existing_refs,
            model.as_deref(),
            model_flag.as_deref(),
            seed_id,
        )?;
        // Persist session in registry
        let session_type = if is_nursery { "nursery" } else { "interactive" };
        self.persist_launched_session(
            &agent,
            &dir,
            args.as_deref(),
            session_type,
            harness_conversation_id.as_deref(),
        );
        // Don't register nursery temp dir as a project — it's ephemeral
        if !is_nursery && crate::domain::project::should_auto_register(Path::new(&dir)) {
            let _ = self.db.register_project_path(Path::new(&dir));
        }
        // The New Session Resume step relaunches a listed session under this
        // fresh agent id: carry the source session's persisted "protocol
        // sent" flag over, or the inherited conversation would receive the
        // block a second time (FR3).
        if let Some(source) = dialog.selected_resume_session.as_ref() {
            self.inherit_protocol_flag(&source.id, &agent.id);
        }
        let spawned_name = agent.name.clone();
        self.interactive_agents.push(agent);
        self.whimsg
            .notify_event(crate::tui::whimsg::events::WhimEvent::new(
                crate::tui::whimsg::events::EventKind::AgentSpawned,
                spawned_name,
            ));
        Ok(())
    }

    /// The harness conversation id a launch reports, paired with the canopy
    /// session that conversation already belongs to (if any).
    ///
    /// It comes from the canopy session being resumed, or from the entry the
    /// user picked out of the CLI's own session list. When canopy already knows
    /// that conversation, the launch adopts that session's id and name, so a
    /// resume stays one session — never inferred from workdir, model or timing.
    fn resolve_launch_identity(
        &self,
        dialog: &NewAgentDialog,
    ) -> (
        Option<String>,
        Option<crate::db::session::InteractiveSession>,
    ) {
        let harness_conversation_id = dialog
            .selected_resume_session
            .as_ref()
            .and_then(|session| session.harness_conversation_id.clone())
            .or_else(|| {
                dialog
                    .selected_session
                    .as_ref()
                    .map(|(id, _label)| id.clone())
            });
        let known_session = harness_conversation_id.as_deref().and_then(|harness_id| {
            self.db
                .find_session_by_harness_id(harness_id)
                .ok()
                .flatten()
        });
        (harness_conversation_id, known_session)
    }

    /// Register the launched session in the registry, reusing an earlier row
    /// when the harness conversation id matches one canopy has seen (see
    /// `Database::register_interactive_session`).
    fn persist_launched_session(
        &self,
        agent: &crate::tui::agent::InteractiveAgent,
        dir: &str,
        args: Option<&str>,
        session_type: &str,
        harness_conversation_id: Option<&str>,
    ) {
        let boot_id = crate::system::boot_id();
        let registration = crate::db::session::SessionRegistration {
            id: &agent.id,
            name: &agent.name,
            cli: agent.cli.as_str(),
            working_dir: dir,
            args,
            pid: agent.pid(),
            session_type,
            boot_id: boot_id.as_deref(),
            harness_conversation_id,
        };
        match self.db.register_interactive_session(&registration) {
            Ok(crate::db::session::RegistrationOutcome::Reactivated { id, .. })
                if id != agent.id =>
            {
                tracing::warn!(
                    "Harness conversation matched session '{id}' but the launched process has \
                     id '{}'; the registry keeps the matched session",
                    agent.id
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Failed to register interactive session: {e}"),
        }
    }

    pub(super) fn launch_scheduled(
        &mut self,
        dialog: &NewAgentDialog,
        model: Option<String>,
    ) -> Result<()> {
        use chrono::Utc;
        if dialog.prompt.is_empty() {
            return Ok(());
        }
        let cli = dialog.selected_cli();
        let id = new_short_id("agent");
        let working_dir = if dialog.working_dir.is_empty() {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/".to_string())
        } else {
            dialog.working_dir.clone()
        };
        let log_path = agent_log_path(&id);
        let agent = crate::domain::models::Agent {
            id,
            prompt: dialog.prompt.clone(),
            trigger: Some(crate::domain::models::Trigger::Cron {
                schedule_expr: dialog.cron_expr.clone(),
            }),
            cli,
            model,
            effort: None,
            working_dir: Some(working_dir),
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path,
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        };
        self.db.upsert_agent(&agent)?;
        if let Some(workdir) = agent.working_dir.as_deref() {
            if crate::domain::project::should_auto_register(Path::new(workdir)) {
                let _ = self.db.register_project_path(Path::new(workdir));
            }
        }
        Ok(())
    }

    pub(super) fn launch_watcher(
        &mut self,
        dialog: &NewAgentDialog,
        model: Option<String>,
    ) -> Result<()> {
        use chrono::Utc;
        if dialog.prompt.is_empty() || dialog.watch_path.is_empty() {
            return Ok(());
        }
        let cli = dialog.selected_cli();
        let id = new_short_id("watch");
        let events: Vec<_> = dialog
            .watch_events
            .iter()
            .filter_map(|e| crate::domain::models::WatchEvent::from_str(e))
            .collect();
        if events.is_empty() {
            return Ok(());
        }
        let log_path = agent_log_path(&id);
        let agent = crate::domain::models::Agent {
            id,
            prompt: dialog.prompt.clone(),
            trigger: Some(crate::domain::models::Trigger::Watch {
                path: dialog.watch_path.clone(),
                events,
                debounce_seconds: 5,
                recursive: false,
            }),
            cli,
            model,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path,
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        };
        self.db.upsert_agent(&agent)?;
        Ok(())
    }

    pub(super) fn launch_terminal(&mut self, dialog: &NewAgentDialog) -> Result<()> {
        use crate::tui::agent::InteractiveAgent;

        let shell = dialog.selected_shell();
        let dir = dialog.working_dir.clone();
        let (cols, rows) = pty_dimensions(self.last_panel_inner);
        let existing_refs: Vec<&str> = self
            .terminal_agents
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        let agent = InteractiveAgent::spawn_terminal(
            shell,
            &dir,
            cols,
            rows,
            None,
            &existing_refs,
            self.theme.header_color,
        )?;
        let _ = self
            .db
            .insert_terminal_session(&agent.id, &agent.name, shell, &dir);
        if crate::domain::project::should_auto_register(Path::new(&dir)) {
            let _ = self.db.register_project_path(Path::new(&dir));
        }
        // Load command history into cache
        let hist = crate::tui::terminal_history::load_history(&self.data_dir, &agent.name);
        agent.replay_scrollback_lines(&hist.scrollback);
        self.terminal_histories.insert(agent.name.clone(), hist);
        let spawned_name = agent.name.clone();
        self.terminal_agents.push(agent);
        self.whimsg
            .notify_event(crate::tui::whimsg::events::WhimEvent::new(
                crate::tui::whimsg::events::EventKind::AgentSpawned,
                spawned_name,
            ));
        Ok(())
    }
}

/// Launch args for an interactive agent: the dialog's base args, with the yolo
/// flag appended when yolo mode is on.
fn yolo_launch_args(dialog: &NewAgentDialog) -> Option<String> {
    let base_args = dialog.selected_args();
    if !dialog.yolo_mode {
        return base_args;
    }
    match dialog.selected_yolo_flag() {
        Some(flag) => Some(match base_args {
            Some(a) => format!("{a} {flag}"),
            None => flag,
        }),
        None => base_args,
    }
}
