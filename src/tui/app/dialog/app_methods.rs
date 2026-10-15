use super::super::types::AgentEntry;
use super::super::types::App;
use super::background_action::BackgroundAction;
use super::new_agent::{BackgroundTrigger, NewAgentDialog, NewTaskType};
use super::prompt::SimplePromptDialog;
use crate::domain::models::Trigger;

#[cfg(test)]
use super::launchpad::LaunchpadDialog;
#[cfg(test)]
use crate::application::ports::AgentRepository;

mod launch;
mod launch_send;

impl App {
    pub fn open_edit_dialog(&mut self) {
        let prev_focus = self.focus;
        let Some(agent) = self.agents.get(self.selected) else {
            return;
        };
        let agent_dir = match agent {
            AgentEntry::Agent(a) => a.working_dir.as_deref(),
            _ => None,
        };
        let AgentEntry::Agent(a) = agent else {
            return; // editing not supported for Interactive/Terminal/Group
        };
        let mut dialog = NewAgentDialog::new(agent_dir);
        dialog.send_target_names = NewAgentDialog::send_target_names(&self.session_names());
        dialog.prev_focus = Some(prev_focus);
        populate_dialog_from_agent(&mut dialog, a);
        dialog.refresh_model_suggestions();
        self.new_agent_dialog = Some(dialog);
        self.focus = super::super::types::Focus::NewAgentDialog;
    }

    pub fn open_new_agent_dialog(&mut self) {
        let prev_focus = self.focus;

        // Get working dir from current agent if available
        let agent_dir = self.selected_agent().and_then(|entry| match entry {
            AgentEntry::Interactive(idx) => self
                .interactive_agents
                .get(*idx)
                .map(|a| a.working_dir.as_str()),
            AgentEntry::Terminal(idx) => self
                .terminal_agents
                .get(*idx)
                .map(|a| a.working_dir.as_str()),
            _ => None,
        });

        let mut dialog = NewAgentDialog::new(agent_dir);
        dialog.send_target_names = NewAgentDialog::send_target_names(&self.session_names());
        dialog.prev_focus = Some(prev_focus);
        self.new_agent_dialog = Some(dialog);
        self.focus = super::super::types::Focus::NewAgentDialog;
    }

    /// Live interactive session names, in display order — the same list the
    /// scheduled-send prompt's target picker uses.
    fn session_names(&self) -> Vec<String> {
        self.interactive_agents
            .iter()
            .map(|agent| agent.name.clone())
            .collect()
    }

    pub fn close_new_agent_dialog(&mut self) {
        if let Some(dialog) = &self.new_agent_dialog {
            if let Some(prev) = dialog.prev_focus {
                self.focus = prev;
            } else {
                self.focus = super::super::types::Focus::Home;
            }
        } else {
            self.focus = super::super::types::Focus::Home;
        }
        self.new_agent_dialog = None;
    }

    pub fn close_launchpad_dialog(&mut self) {
        let prev_focus = self
            .pending_launch_dialog
            .as_ref()
            .and_then(|dialog| dialog.prev_focus)
            .unwrap_or(super::super::types::Focus::Preview);
        self.launchpad_dialog = None;
        self.pending_launch_dialog = None;
        self.focus = prev_focus;
    }

    /// Open prompt template dialog with the specified template and optional initial content.
    /// Restores any persisted session for the current workdir.
    /// Injects an invisible system block on every prompt: per-turn workspace
    /// context always, plus the session-start protocol on the first prompt
    /// of a session (idempotent per `App::current_prompt_session_key`).
    pub fn open_simple_prompt_dialog(
        &mut self,
        initial_content: Option<std::collections::HashMap<String, String>>,
    ) {
        let prev_focus = self.focus;
        let workdir = self.current_workdir();
        let session_key = self.current_prompt_session_key();
        let mut dialog = SimplePromptDialog::new();

        // Restore persisted session for this agent/session if available
        if let Some(session) = self.prompt_builder_sessions.get(&session_key) {
            session.restore_into(&mut dialog);
        }
        let current_project_path = self
            .db
            .get_project_by_path_or_ancestor(&workdir)
            .ok()
            .flatten()
            .map(|project| project.path);
        dialog.migrate_legacy_sections(current_project_path.as_deref());

        // The per-turn context (workspace/active missions/recent chatter) is
        // worth its tokens every send, so it is never gated. The session-start
        // protocol ("[START HERE — required]", the tool-usage contract, the
        // skills list) is a one-time opening instruction: send it on the
        // first prompt of a session only.
        let state = self
            .session_protocol_state
            .get(&session_key)
            .cloned()
            .unwrap_or_default();
        let should_send_protocol = !state.protocol_sent && self.active_sandbox.is_none();

        let turn_context = self.build_turn_context_block();
        let content = if should_send_protocol {
            format!("{}\n\n{turn_context}", self.build_session_protocol_block())
        } else {
            turn_context
        };
        dialog.system_content = Some(content);
        dialog.protocol_included = should_send_protocol;

        if let Some(content) = initial_content {
            for (section_name, section_content) in content {
                if section_name == "instruction" || section_name.starts_with("instruction_") {
                    let instr_id = dialog
                        .enabled_sections
                        .iter()
                        .find(|s| *s == "instruction" || s.starts_with("instruction_"))
                        .cloned()
                        .unwrap_or_else(|| "instruction_1".to_string());
                    let char_len = section_content.chars().count();
                    dialog.sections.insert(instr_id.clone(), section_content);
                    dialog.section_cursors.insert(instr_id, char_len);
                } else if section_name == "context" || section_name.starts_with("context_") {
                    // Context sections from initial_content are locked.
                    let ctx_id = dialog.add_section_with_content(&section_name, section_content);
                    dialog.lock_section(&ctx_id);
                } else {
                    dialog.add_section_with_content(&section_name, section_content);
                }
            }
        }
        dialog.migrate_legacy_sections(current_project_path.as_deref());
        // Every open path lands the cursor in the first instruction, ready to
        // type; the send control (focus 0) is the last stop of the cycle, so it
        // must never be the initial focus — not on a fresh open, a reopen, or a
        // restored/recalled session.
        dialog.focus_first_section();
        dialog.prev_focus = Some(prev_focus);
        self.simple_prompt_dialog = Some(dialog);
        self.focus = super::super::types::Focus::PromptTemplateDialog;
    }

    /// Build the per-turn context block: workspace path, active missions,
    /// and recent chatter. Genuinely changes turn to turn, so it is sent
    /// with every prompt — never gated by session-start idempotency.
    fn build_turn_context_block(&self) -> String {
        let mut lines: Vec<String> = Vec::new();

        let (workdir, intents, chatter) = self.build_system_context_parts();
        lines.push(format!("workspace: {workdir}"));
        Self::push_intents(&mut lines, &intents);
        Self::push_chatter(&mut lines, &chatter);

        lines.join("\n")
    }

    /// Build the session-start protocol block: the opening contract
    /// ("[START HERE — required]", the tool-usage protocol, the skills
    /// list). Static across a session — sent once, not per turn.
    ///
    /// Single source of truth: [`crate::domain::sandbox::canopy_protocol_block`],
    /// so the text the promptbuilder injects (non-sandbox path) and the text
    /// materialised into a sandbox's instruction file can never drift apart.
    fn build_session_protocol_block(&self) -> String {
        crate::domain::sandbox::canopy_protocol_block().to_string()
    }

    /// Extract workdir, intents, and chatter from activity state or fallback.
    fn build_system_context_parts(
        &self,
    ) -> (
        String,
        Vec<crate::domain::sync::ActiveIntent>,
        Vec<crate::domain::sync::SyncMessage>,
    ) {
        if let Some(state) = self.selected_activity_state() {
            let chatter = state
                .recent_messages
                .iter()
                .filter(|m| m.kind.is_chatter())
                .take(5)
                .cloned()
                .collect();
            (state.workdir.clone(), state.active_intents, chatter)
        } else {
            let workdir = self.current_workdir().to_string_lossy().to_string();
            let intents = self.active_missions_for_workdir(&workdir);
            (workdir, intents, Vec::new())
        }
    }

    fn push_intents(lines: &mut Vec<String>, intents: &[crate::domain::sync::ActiveIntent]) {
        if intents.is_empty() {
            return;
        }
        lines.push("active missions:".to_string());
        for intent in intents {
            lines.push(format!(
                "  - {} [{}] {}: {}",
                intent.agent_name,
                intent.impact.as_str(),
                intent.mission,
                intent.description
            ));
        }
    }

    fn push_chatter(lines: &mut Vec<String>, chatter: &[crate::domain::sync::SyncMessage]) {
        if chatter.is_empty() {
            return;
        }
        lines.push("recent messages:".to_string());
        for msg in chatter {
            lines.push(format!("  - {}: {}", msg.agent_name, msg.message));
        }
    }

    /// Build a compact sync context string from active intents and recent chatter.
    /// Kept for backwards compatibility; not used by the prompt builder any more.
    #[allow(dead_code)]
    fn build_sync_context_text(&self) -> Option<String> {
        let state = self.selected_activity_state()?;
        let mut lines = Vec::new();

        lines.push(format!(
            "workspace: {} | agents: {} | vibe: {}",
            state.workdir,
            state.participant_count,
            state.vibe.as_str()
        ));

        if !state.active_intents.is_empty() {
            lines.push("active missions:".to_string());
            for intent in &state.active_intents {
                lines.push(format!(
                    "  - {} [{}] {}: {}",
                    intent.agent_name,
                    intent.impact.as_str(),
                    intent.mission,
                    intent.description
                ));
            }
        }

        let chatter: Vec<_> = state
            .recent_messages
            .iter()
            .filter(|m| m.kind.is_chatter())
            .take(5)
            .collect();
        if !chatter.is_empty() {
            lines.push("recent messages:".to_string());
            for msg in chatter {
                lines.push(format!("  - {}: {}", msg.agent_name, msg.message));
            }
        }

        Some(lines.join("\n"))
    }

    /// Close simple prompt dialog and persist its state for the current workdir.
    pub fn close_simple_prompt_dialog(&mut self) {
        self._close_simple_prompt_dialog(true);
    }

    /// Close simple prompt dialog without persisting its state (e.g. after sending).
    pub fn discard_simple_prompt_dialog(&mut self) {
        self._close_simple_prompt_dialog(false);
    }

    fn _close_simple_prompt_dialog(&mut self, persist: bool) {
        if let Some(dialog) = self.simple_prompt_dialog.take() {
            if let Some(prev) = dialog.prev_focus {
                self.focus = prev;
            } else {
                self.focus = super::super::types::Focus::Agent;
            }
            if persist {
                let session_key = self.current_prompt_session_key();
                let session = super::prompt::PromptBuilderSession::from_dialog(&dialog);
                self.prompt_builder_sessions.insert(session_key, session);
            }
        } else {
            self.focus = super::super::types::Focus::Agent;
        }
    }
}

// ── Free helpers ──────────────────────────────────────────────────

fn new_short_id(prefix: &str) -> String {
    format!("{}-{}", prefix, &uuid::Uuid::new_v4().to_string()[..8])
}

fn agent_log_path(id: &str) -> String {
    dirs::home_dir()
        .map(|h| h.join(".canopy/logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/canopy/logs"))
        .join(id)
        .with_extension("log")
        .to_string_lossy()
        .to_string()
}

/// Apply a cron-agent edit dialog's fields onto an existing agent in place.
/// Pure (no I/O) so the mapping can be unit-tested without a `Database`.
fn apply_scheduled_edit(
    agent: &mut crate::domain::models::Agent,
    dialog: &NewAgentDialog,
    model: Option<&str>,
) {
    agent.prompt = dialog.prompt.clone();
    if let Some(Trigger::Cron { schedule_expr }) = &mut agent.trigger {
        *schedule_expr = dialog.cron_expr.clone();
    }
    // A send_to_session agent has no CLI/model — a TUI edit keeps them unset.
    if !dialog.is_send_to_session() {
        agent.cli = dialog.selected_cli();
        agent.model = model.map(String::from);
    }
    agent.working_dir = if dialog.working_dir.is_empty() {
        None
    } else {
        Some(dialog.working_dir.clone())
    };
}

/// Apply a watch-agent edit dialog's fields onto an existing agent in place.
/// Leaves `debounce_seconds`/`recursive` untouched — the dialog does not
/// expose them for editing (yet), so the agent keeps its prior values.
/// Pure (no I/O) so the mapping can be unit-tested without a `Database`.
fn apply_watcher_edit(
    agent: &mut crate::domain::models::Agent,
    dialog: &NewAgentDialog,
    model: Option<&str>,
) {
    agent.prompt = dialog.prompt.clone();
    if !dialog.is_send_to_session() {
        agent.cli = dialog.selected_cli();
        agent.model = model.map(String::from);
    }
    if let Some(Trigger::Watch { path, events, .. }) = &mut agent.trigger {
        *path = dialog.watch_path.clone();
        *events =
            crate::domain::models::WatchEvent::parse_list(&dialog.watch_events).unwrap_or_default();
    }
}

/// Populate a `NewAgentDialog` from an existing agent's fields.
fn populate_dialog_from_agent(dialog: &mut NewAgentDialog, a: &crate::domain::models::Agent) {
    dialog.edit_id = Some(a.id.clone());
    dialog.task_type = NewTaskType::Background;
    dialog.prompt = a.prompt.clone();
    dialog.prompt_cursor = a.prompt.chars().count();
    dialog.prompt_scroll = 0;
    dialog.model = a.model.clone().unwrap_or_default();
    dialog.working_dir = a.working_dir.clone().unwrap_or_default();
    dialog.field = 2;

    if a.is_send_to_session() {
        dialog.background_action = BackgroundAction::SendToSession;
        if let Some(name) = a.action.target_session_name() {
            if let Some(idx) = dialog.send_target_names.iter().position(|n| n == name) {
                dialog.send_target_idx = idx;
            }
        }
    }

    if let Some(idx) = dialog
        .available_clis
        .iter()
        .position(|c| c.as_str() == a.cli.as_str())
    {
        dialog.cli_index = idx;
    }

    match &a.trigger {
        Some(crate::domain::models::Trigger::Cron { schedule_expr }) => {
            dialog.background_trigger = BackgroundTrigger::Cron;
            dialog.cron_expr = schedule_expr.clone();
        }
        Some(crate::domain::models::Trigger::Watch { path, events, .. }) => {
            dialog.background_trigger = BackgroundTrigger::Watch;
            dialog.watch_path = path.clone();
            dialog.watch_events = events
                .iter()
                .map(|e| e.to_string().to_lowercase())
                .collect();
        }
        None => {
            dialog.background_trigger = BackgroundTrigger::Cron;
        }
    }
}

/// Resolve PTY dimensions from the last known panel size, falling back to terminal size.
fn pty_dimensions(last_panel_inner: (u16, u16)) -> (u16, u16) {
    if last_panel_inner != (0, 0) {
        return last_panel_inner;
    }
    let (tw, th) = ratatui::crossterm::terminal::size().unwrap_or((120, 40));
    (tw.saturating_sub(28), th.saturating_sub(4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli, Trigger, WatchEvent};
    use crate::tui::app::types::Focus;
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn cron_agent(id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "original prompt".to_string(),
            trigger: Some(Trigger::Cron {
                schedule_expr: "0 9 * * *".to_string(),
            }),
            cli: Cli::new("claude"),
            model: Some("original-model".to_string()),
            effort: None,
            working_dir: Some("/original/dir".to_string()),
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: "/tmp/test-cron.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 3,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn watch_agent(id: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "original prompt".to_string(),
            trigger: Some(Trigger::Watch {
                path: "/original/watch".to_string(),
                events: vec![WatchEvent::Create],
                debounce_seconds: 42,
                recursive: true,
            }),
            cli: Cli::new("claude"),
            model: Some("original-model".to_string()),
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: "/tmp/test-watch.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 7,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn dialog_with_clis(agent: &Agent) -> NewAgentDialog {
        let mut dialog = NewAgentDialog::new(Some("/tmp"));
        dialog.available_clis = vec![Cli::new("opencode"), agent.cli.clone(), Cli::new("codex")];
        dialog.cli_configs = vec![None, None, None];
        dialog
    }

    #[test]
    fn populate_dialog_from_agent_prefills_cron_agent_fields() {
        let agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);

        populate_dialog_from_agent(&mut dialog, &agent);

        assert_eq!(dialog.edit_id.as_deref(), Some("cron-1"));
        assert!(dialog.is_edit_mode());
        assert!(matches!(dialog.task_type, NewTaskType::Background));
        assert!(matches!(dialog.background_trigger, BackgroundTrigger::Cron));
        assert_eq!(dialog.prompt, "original prompt");
        assert_eq!(dialog.model, "original-model");
        assert_eq!(dialog.working_dir, "/original/dir");
        assert_eq!(dialog.cron_expr, "0 9 * * *");
        assert_eq!(dialog.selected_cli().as_str(), "claude");
    }

    #[test]
    fn populate_dialog_from_agent_prefills_watch_agent_fields() {
        let agent = watch_agent("watch-1");
        let mut dialog = dialog_with_clis(&agent);

        populate_dialog_from_agent(&mut dialog, &agent);

        assert_eq!(dialog.edit_id.as_deref(), Some("watch-1"));
        assert!(matches!(
            dialog.background_trigger,
            BackgroundTrigger::Watch
        ));
        assert_eq!(dialog.watch_path, "/original/watch");
        assert_eq!(dialog.watch_events, vec!["create".to_string()]);
    }

    #[test]
    fn apply_scheduled_edit_updates_editable_fields_without_touching_the_rest() {
        let mut agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "updated prompt".to_string();
        dialog.cron_expr = "5 6 * * *".to_string();
        dialog.working_dir = "/updated/dir".to_string();
        dialog.set_cli_index(2); // "codex"

        apply_scheduled_edit(&mut agent, &dialog, Some("updated-model"));

        assert_eq!(agent.prompt, "updated prompt");
        assert_eq!(agent.model.as_deref(), Some("updated-model"));
        assert_eq!(agent.working_dir.as_deref(), Some("/updated/dir"));
        assert_eq!(agent.cli.as_str(), "codex");
        assert!(
            matches!(&agent.trigger, Some(Trigger::Cron { schedule_expr }) if schedule_expr == "5 6 * * *")
        );
        // Fields the dialog never touches must survive the edit untouched.
        assert_eq!(agent.id, "cron-1");
        assert_eq!(agent.trigger_count, 3);
    }

    #[test]
    fn apply_scheduled_edit_clears_working_dir_when_dialog_field_is_empty() {
        let mut agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "still needed".to_string();
        dialog.working_dir = String::new();

        apply_scheduled_edit(&mut agent, &dialog, None);

        assert_eq!(agent.working_dir, None);
    }

    #[test]
    fn apply_watcher_edit_updates_path_and_events_but_preserves_debounce_and_recursive() {
        let mut agent = watch_agent("watch-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "updated prompt".to_string();
        dialog.watch_path = "/updated/watch".to_string();
        dialog.watch_events = vec!["modify".to_string(), "delete".to_string()];

        apply_watcher_edit(&mut agent, &dialog, Some("updated-model"));

        let Some(Trigger::Watch {
            path,
            events,
            debounce_seconds,
            recursive,
        }) = &agent.trigger
        else {
            panic!("expected a Watch trigger");
        };
        assert_eq!(path, "/updated/watch");
        assert_eq!(events, &vec![WatchEvent::Modify, WatchEvent::Delete]);
        // Not exposed by the dialog yet — must survive the edit unchanged.
        assert_eq!(*debounce_seconds, 42);
        assert!(*recursive);
        assert_eq!(agent.prompt, "updated prompt");
        assert_eq!(agent.model.as_deref(), Some("updated-model"));
    }

    #[test]
    fn open_edit_dialog_prefills_from_the_selected_background_agent() {
        let db = test_db();
        let agent = cron_agent("cron-1");
        db.upsert_agent(&agent).expect("seed agent");

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(agent)];
        app.selected = 0;

        app.open_edit_dialog();

        let dialog = app.new_agent_dialog.as_ref().expect("dialog should open");
        assert_eq!(dialog.edit_id.as_deref(), Some("cron-1"));
        assert_eq!(dialog.prompt, "original prompt");
        assert!(matches!(app.focus, Focus::NewAgentDialog));
    }

    #[test]
    fn cancelling_the_edit_dialog_does_not_persist_changes() {
        let db = test_db();
        let agent = cron_agent("cron-1");
        db.upsert_agent(&agent).expect("seed agent");

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(agent)];
        app.selected = 0;

        app.open_edit_dialog();
        app.new_agent_dialog.as_mut().unwrap().prompt = "mutated but never saved".to_string();
        app.close_new_agent_dialog();

        assert!(app.new_agent_dialog.is_none());
        let stored = db.get_agent("cron-1").unwrap().expect("agent still exists");
        assert_eq!(stored.prompt, "original prompt");
    }

    #[test]
    fn confirming_the_edit_dialog_persists_prompt_and_model_changes() {
        let db = test_db();
        let agent = cron_agent("cron-1");
        db.upsert_agent(&agent).expect("seed agent");

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(agent)];
        app.selected = 0;

        app.open_edit_dialog();
        {
            let dialog = app.new_agent_dialog.as_mut().unwrap();
            dialog.prompt = "updated prompt".to_string();
            dialog.model = "updated-model".to_string();
        }
        app.launch_new_agent().expect("save edit");

        assert!(app.new_agent_dialog.is_none());
        let stored = db.get_agent("cron-1").unwrap().expect("agent still exists");
        assert_eq!(stored.prompt, "updated prompt");
        assert_eq!(stored.model.as_deref(), Some("updated-model"));
    }

    // ── Free helper tests ────────────────────────────────────────

    #[test]
    fn new_short_id_has_prefix() {
        let id = new_short_id("agent");
        assert!(id.starts_with("agent-"));
        assert!(id.len() > "agent-".len());
    }

    #[test]
    fn new_short_id_unique() {
        let id1 = new_short_id("test");
        let id2 = new_short_id("test");
        assert_ne!(id1, id2);
    }

    #[test]
    fn agent_log_path_ends_with_log_extension() {
        let path = agent_log_path("agent-123");
        assert!(path.ends_with(".log"));
        assert!(path.contains("agent-123"));
    }

    #[test]
    fn pty_dimensions_fallback_to_terminal_size() {
        let (cols, rows) = pty_dimensions((0, 0));
        assert!(cols > 0);
        assert!(rows > 0);
    }

    #[test]
    fn pty_dimensions_uses_panel_size_when_available() {
        let (cols, rows) = pty_dimensions((100, 50));
        assert_eq!(cols, 100);
        assert_eq!(rows, 50);
    }

    // ── apply_scheduled_edit edge cases ──────────────────────────

    #[test]
    fn apply_scheduled_edit_empty_cron_preserves() {
        let mut agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "test".to_string();
        dialog.cron_expr = String::new();
        apply_scheduled_edit(&mut agent, &dialog, None);
        if let Some(Trigger::Cron { schedule_expr }) = &agent.trigger {
            assert!(schedule_expr.is_empty());
        } else {
            panic!("expected Cron trigger");
        }
    }

    #[test]
    fn apply_scheduled_edit_model_none() {
        let mut agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "test".to_string();
        apply_scheduled_edit(&mut agent, &dialog, None);
        assert!(agent.model.is_none());
    }

    #[test]
    fn apply_scheduled_edit_model_some() {
        let mut agent = cron_agent("cron-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "test".to_string();
        apply_scheduled_edit(&mut agent, &dialog, Some("gpt-4"));
        assert_eq!(agent.model.as_deref(), Some("gpt-4"));
    }

    // ── apply_watcher_edit edge cases ────────────────────────────

    #[test]
    fn apply_watcher_edit_model_none() {
        let mut agent = watch_agent("watch-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "test".to_string();
        dialog.watch_path = "/new/path".to_string();
        dialog.watch_events = vec!["modify".to_string()];
        apply_watcher_edit(&mut agent, &dialog, None);
        assert!(agent.model.is_none());
    }

    #[test]
    fn apply_watcher_edit_model_some() {
        let mut agent = watch_agent("watch-1");
        let mut dialog = dialog_with_clis(&agent);
        dialog.prompt = "test".to_string();
        dialog.watch_path = "/new/path".to_string();
        dialog.watch_events = vec!["modify".to_string()];
        apply_watcher_edit(&mut agent, &dialog, Some("gpt-4"));
        assert_eq!(agent.model.as_deref(), Some("gpt-4"));
    }

    // ── populate_dialog_from_agent edge cases ────────────────────

    #[test]
    fn populate_dialog_from_agent_no_trigger() {
        let mut agent = cron_agent("cron-1");
        agent.trigger = None;
        let mut dialog = dialog_with_clis(&agent);
        populate_dialog_from_agent(&mut dialog, &agent);
        assert!(matches!(dialog.background_trigger, BackgroundTrigger::Cron));
    }

    // ── close_new_agent_dialog ───────────────────────────────────

    #[test]
    fn close_new_agent_dialog_with_prev_focus() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = NewAgentDialog::new(None);
        dialog.prev_focus = Some(Focus::Agent);
        app.new_agent_dialog = Some(dialog);
        app.close_new_agent_dialog();
        assert!(app.new_agent_dialog.is_none());
        assert!(matches!(app.focus, Focus::Agent));
    }

    #[test]
    fn close_new_agent_dialog_without_prev_focus() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let dialog = NewAgentDialog::new(None);
        app.new_agent_dialog = Some(dialog);
        app.close_new_agent_dialog();
        assert!(app.new_agent_dialog.is_none());
        assert!(matches!(app.focus, Focus::Home));
    }

    #[test]
    fn close_new_agent_dialog_none() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.close_new_agent_dialog();
        assert!(matches!(app.focus, Focus::Home));
    }

    // ── close_launchpad_dialog ───────────────────────────────────

    #[test]
    fn close_launchpad_dialog_clears_both() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Set up both dialogs via the normal open path
        app.new_agent_dialog = Some(NewAgentDialog::new(None));
        app.launchpad_dialog = app
            .new_agent_dialog
            .as_ref()
            .and_then(|d| LaunchpadDialog::for_workdir(&db, &d.working_dir).ok());
        app.pending_launch_dialog = app.new_agent_dialog.take();
        app.close_launchpad_dialog();
        assert!(app.launchpad_dialog.is_none());
        assert!(app.pending_launch_dialog.is_none());
    }

    // ── close_simple_prompt_dialog ───────────────────────────────

    #[test]
    fn close_simple_prompt_dialog_persists_session() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "test prompt".to_string());
        dialog.prev_focus = Some(Focus::Agent);
        app.simple_prompt_dialog = Some(dialog);

        app.close_simple_prompt_dialog();
        assert!(app.simple_prompt_dialog.is_none());
        assert!(matches!(app.focus, Focus::Agent));
        // Session should be persisted
        let key = app.current_prompt_session_key();
        assert!(app.prompt_builder_sessions.contains_key(&key));
    }

    #[test]
    fn discard_simple_prompt_dialog_does_not_persist() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "test prompt".to_string());
        dialog.prev_focus = Some(Focus::Agent);
        app.simple_prompt_dialog = Some(dialog);

        app.discard_simple_prompt_dialog();
        assert!(app.simple_prompt_dialog.is_none());
        let key = app.current_prompt_session_key();
        assert!(!app.prompt_builder_sessions.contains_key(&key));
    }

    #[test]
    fn close_simple_prompt_dialog_no_dialog() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.close_simple_prompt_dialog();
        assert!(matches!(app.focus, Focus::Agent));
    }

    // ── launch_scheduled edge cases ──────────────────────────────

    #[test]
    fn launch_scheduled_empty_prompt_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = NewAgentDialog::new(None);
        dialog.prompt = String::new();
        dialog.task_type = NewTaskType::Background;
        dialog.background_trigger = BackgroundTrigger::Cron;
        app.launch_scheduled(&dialog, None)
            .expect("should not error");
        // No agent should have been created
        let agents = db.list_agents().unwrap();
        assert!(agents.is_empty());
    }

    // ── launch_watcher edge cases ────────────────────────────────

    #[test]
    fn launch_watcher_empty_prompt_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = NewAgentDialog::new(None);
        dialog.prompt = String::new();
        dialog.watch_path = "/tmp".to_string();
        dialog.task_type = NewTaskType::Background;
        dialog.background_trigger = BackgroundTrigger::Watch;
        app.launch_watcher(&dialog, None).expect("should not error");
        let agents = db.list_agents().unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn launch_watcher_empty_watch_path_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = NewAgentDialog::new(None);
        dialog.prompt = "test".to_string();
        dialog.watch_path = String::new();
        dialog.task_type = NewTaskType::Background;
        dialog.background_trigger = BackgroundTrigger::Watch;
        app.launch_watcher(&dialog, None).expect("should not error");
        let agents = db.list_agents().unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn launch_watcher_empty_events_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut dialog = NewAgentDialog::new(None);
        dialog.prompt = "test".to_string();
        dialog.watch_path = "/tmp".to_string();
        dialog.watch_events.clear();
        dialog.task_type = NewTaskType::Background;
        dialog.background_trigger = BackgroundTrigger::Watch;
        app.launch_watcher(&dialog, None).expect("should not error");
        let agents = db.list_agents().unwrap();
        assert!(agents.is_empty());
    }

    // ── launch_new_agent edge cases ──────────────────────────────

    #[test]
    fn launch_new_agent_no_dialog_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.launch_new_agent().expect("should not error");
        assert!(app.new_agent_dialog.is_none());
    }

    // ── open_new_agent_dialog ────────────────────────────────────

    #[test]
    fn open_new_agent_dialog_sets_focus() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.open_new_agent_dialog();
        assert!(app.new_agent_dialog.is_some());
        assert!(matches!(app.focus, Focus::NewAgentDialog));
    }

    #[test]
    fn open_new_agent_dialog_captures_prev_focus() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.focus = Focus::Agent;
        app.open_new_agent_dialog();
        let dialog = app.new_agent_dialog.as_ref().unwrap();
        assert!(matches!(dialog.prev_focus, Some(Focus::Agent)));
    }

    // ── open_edit_dialog edge cases ──────────────────────────────

    #[test]
    fn open_edit_dialog_no_selection_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.open_edit_dialog();
        assert!(app.new_agent_dialog.is_none());
    }

    #[test]
    fn open_edit_dialog_non_agent_entry_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Add a non-agent entry (terminal index 0, but no terminal agents exist)
        app.agents = vec![AgentEntry::Terminal(0)];
        app.selected = 0;
        app.open_edit_dialog();
        assert!(app.new_agent_dialog.is_none());
    }

    // ── push_intents / push_chatter ─────────────────────────────

    #[test]
    fn push_intents_empty_does_not_add_header() {
        let mut lines = Vec::new();
        App::push_intents(&mut lines, &[]);
        assert!(lines.is_empty());
    }

    #[test]
    fn push_intents_adds_header_and_items() {
        use crate::domain::sync::{ActiveIntent, MissionImpact, WorkspaceStatus};
        let mut lines = Vec::new();
        let intents = vec![ActiveIntent {
            agent_id: "agent-a-id".to_string(),
            agent_name: "agent-a".to_string(),
            impact: MissionImpact::High,
            mission: "deploy".to_string(),
            description: "ship it".to_string(),
            status: WorkspaceStatus::Stable,
            since: Utc::now().timestamp(),
        }];
        App::push_intents(&mut lines, &intents);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "active missions:");
        assert!(lines[1].contains("agent-a"));
        assert!(lines[1].contains("deploy"));
    }

    #[test]
    fn push_chatter_empty_does_not_add_header() {
        let mut lines = Vec::new();
        App::push_chatter(&mut lines, &[]);
        assert!(lines.is_empty());
    }

    #[test]
    fn push_chatter_adds_header_and_items() {
        use crate::domain::sync::SyncMessage;
        let mut lines = Vec::new();
        let messages = vec![SyncMessage {
            id: 1,
            workdir: "/tmp".to_string(),
            agent_id: "agent-b-id".to_string(),
            agent_name: "agent-b".to_string(),
            kind: crate::domain::sync::MessageKind::Info,
            message: "hello".to_string(),
            payload: None,
            created_at: Utc::now().timestamp(),
        }];
        App::push_chatter(&mut lines, &messages);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "recent messages:");
        assert!(lines[1].contains("agent-b"));
        assert!(lines[1].contains("hello"));
    }

    // ── update_prompt_config ────────────────────────────────────

    #[test]
    fn update_prompt_config_on_object() {
        let config = serde_json::json!({"prompt_template": "old", "key": "val"});
        let result = App::update_prompt_config(&config, "new prompt");
        assert_eq!(
            result.get("prompt_template").and_then(|v| v.as_str()),
            Some("new prompt")
        );
        assert_eq!(result.get("key").and_then(|v| v.as_str()), Some("val"));
    }

    #[test]
    fn update_prompt_config_on_non_object() {
        let config = serde_json::json!("just a string");
        let result = App::update_prompt_config(&config, "prompt");
        assert_eq!(
            result.get("prompt_template").and_then(|v| v.as_str()),
            Some("prompt")
        );
    }

    #[test]
    fn update_prompt_config_empty_object() {
        let config = serde_json::json!({});
        let result = App::update_prompt_config(&config, "p");
        assert_eq!(
            result.get("prompt_template").and_then(|v| v.as_str()),
            Some("p")
        );
    }

    // ── build_system_context_parts ──────────────────────────────

    #[test]
    fn build_system_context_parts_fallback_when_no_activity() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let (workdir, intents, chatter) = app.build_system_context_parts();
        assert!(!workdir.is_empty());
        assert!(intents.is_empty());
        assert!(chatter.is_empty());
    }

    // ── populate_dialog_from_agent: cli not in available list ───

    #[test]
    fn populate_dialog_from_agent_cli_not_in_list() {
        let mut agent = cron_agent("cron-x");
        agent.cli = Cli::new("unknown-cli");
        let mut dialog = NewAgentDialog::new(None);
        dialog.available_clis = vec![Cli::new("opencode"), Cli::new("claude")];
        dialog.cli_configs = vec![None, None];
        populate_dialog_from_agent(&mut dialog, &agent);
        // cli_index stays at 0 (default) since unknown-cli is not in available_clis
        assert_eq!(dialog.cli_index, 0);
    }

    #[test]
    fn editing_agent_preserves_existing_effort() {
        let db = test_db();
        let mut agent = cron_agent("effort-1");
        agent.effort = Some("high".to_string());
        db.upsert_agent(&agent).expect("seed");

        let data_dir = tempdir().expect("data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("app");
        app.agents = vec![AgentEntry::Agent(agent)];
        app.selected = 0;

        app.open_edit_dialog();
        {
            let dialog = app.new_agent_dialog.as_mut().unwrap();
            dialog.prompt = "changed prompt".to_string();
        }
        app.launch_new_agent().expect("save");

        let stored = db.get_agent("effort-1").unwrap().expect("agent exists");
        assert_eq!(
            stored.effort.as_deref(),
            Some("high"),
            "edit must not clear effort"
        );
    }

    // ── select_agent_by_name ────────────────────────────────────────

    #[test]
    fn select_agent_by_name_sets_selection_when_present() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![
            AgentEntry::Agent(cron_agent("a1")),
            AgentEntry::Agent(cron_agent("a2")),
        ];
        app.select_agent_by_name("a2");
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn select_agent_by_name_ignores_absent_or_empty() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
        app.selected = 0;
        app.select_agent_by_name("not-there");
        assert_eq!(app.selected, 0); // unchanged
        app.select_agent_by_name("");
        assert_eq!(app.selected, 0); // unchanged
    }

    // ── launchpad_mission_choice ────────────────────────────────────

    #[test]
    fn launchpad_mission_choice_continue_requires_selected_mission() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let _app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let launchpad = LaunchpadDialog::for_workdir(&db, "/tmp").unwrap();
        // No mission selected -> None
        assert!(App::launchpad_mission_choice(&launchpad).is_none());
    }

    // ── launchpad_initial_content ───────────────────────────────────

    #[test]
    fn launchpad_initial_content_skips_blank_summary_and_lists_peers() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut launchpad = LaunchpadDialog::for_workdir(&db, "/tmp").unwrap();
        // Simulate an empty mission context and some active peer missions
        launchpad.active_missions =
            vec![crate::tui::app::dialog::launchpad::ActiveMissionSummary {
                agent_name: "peer".into(),
                impact: "low".into(),
                mission: "peer mission".into(),
            }];
        let content = App::launchpad_initial_content(
            &app,
            "/tmp",
            "My Mission",
            Some(""), // blank summary -> should be skipped
            &launchpad,
        );
        assert!(content
            .get("context")
            .unwrap()
            .contains("mission: My Mission"));
        assert!(!content.get("context").unwrap().contains("previous_summary"));
        assert!(content
            .get("context")
            .unwrap()
            .contains("active_peer_missions"));
        assert!(content
            .get("context")
            .unwrap()
            .contains("peer [low]: peer mission"));
    }
}
