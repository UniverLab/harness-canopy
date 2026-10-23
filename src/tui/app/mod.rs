mod agents;
mod context_transfer;
pub(crate) mod crews_calls;
pub(crate) mod crews_dialog;
pub(crate) mod crews_face;
pub(crate) mod crews_logic;
mod data;
pub mod dialog;
mod gamification;
pub(crate) mod graph_live_nav;
pub(crate) mod graph_live_state;
pub(crate) mod hub;
pub(crate) mod hub_settings;
mod init;
pub(crate) mod panel_face;
mod project_activity;
mod project_filter;
mod project_graph;
mod project_nav;
mod refresh;
mod refresh_graph_whimsg;
mod sessions;
mod sidebar_nav;
mod sync;
pub(crate) mod system_monitor;

use anyhow::Result;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::application::notification_service::DefaultNotificationService;
use crate::application::ports::{AgentRepository, StateRepository};
use crate::db::Database;

use super::agent::InteractiveAgent;
use super::context_transfer::{
    build_context_payload_for, initial_capture_units, interactive_capture_kind,
    interactive_line_page_count, interactive_prompt_count, ContextCaptureKind, ContextSourceKind,
    ContextTransferConfig, ContextTransferModal, ContextTransferStep,
};
use crate::domain::graphs::{GraphSpecStatus, GraphStatus};
use crate::tui::prompt_templates::PromptTemplates;

pub(crate) use crate::tui::mcp_client::send_mcp_task_run;

use system_monitor::{
    blend_system_info, load_cli_usage, playground_vector_search, spawn_system_monitor,
};

// ── pub(crate) re-exports for tests (use super::{…} in #[cfg(test)] mods) ─
#[cfg(test)]
pub(crate) use sessions::{
    process_is_alive, process_matches_cli, process_outlives_grace, resume_decision,
    should_resume_session, ResumeDecision,
};
#[cfg(test)]
pub(crate) use system_monitor::{
    adaptive_change_score, adaptive_poll_interval_ms, blend_optional_f32, blend_optional_f64,
    lerp_f32, lerp_u64, sample_from, SystemSample,
};
pub(crate) use system_monitor::{
    calculate_log_hash, log_contains_error, log_contains_spawn, log_contains_success,
};

// ── Types ───────────────────────────────────────────────────────

pub mod session_resume;
pub mod terminal_search;
pub mod types;
pub mod utils;

pub(crate) use session_resume::build_resumed_session_args;
pub use terminal_search::TerminalSearch;
pub(crate) use types::ContextTransferSource;
pub(crate) use types::CtrlCState;
pub(crate) use types::GraphLiveFocus;
pub use types::{
    AgentEntry, AgentSectionFocus, App, AutomationKind, Focus, PanelFace, ProjectTab, SidebarLayer,
};
use types::{GraphSidebarMeta, RagTransferModal, SidebarStepMemory};

impl App {
    pub fn selected_agent(&self) -> Option<&AgentEntry> {
        self.agents.get(self.selected)
    }

    pub fn selected_project(&self) -> Option<&crate::domain::project::Project> {
        self.projects.get(self.selected_project)
    }

    pub fn visible_graphs(&self) -> Vec<&crate::domain::graphs::Graph> {
        if self.graph_view_archived {
            self.archived_graphs.iter().collect()
        } else {
            self.graphs.iter().collect()
        }
    }

    pub fn selected_graph(&self) -> Option<&crate::domain::graphs::Graph> {
        let selected_id = self.selected_graph_id.as_ref()?;
        if self.graph_view_archived {
            self.archived_graphs.iter().find(|lp| lp.id == *selected_id)
        } else {
            self.graphs.iter().find(|lp| lp.id == *selected_id)
        }
    }

    pub fn selected_graph_spec(&self) -> Option<&crate::domain::graphs::GraphSpecDetails> {
        self.graph_details
            .as_ref()
            .and_then(|details| details.specs.get(self.graph_selected_spec))
    }

    pub fn selected_graph_node(&self) -> Option<&crate::domain::graphs::GraphNode> {
        self.selected_graph_spec()
            .and_then(|spec| spec.nodes.get(self.graph_selected_node))
    }

    /// Latest run info (status/started_at/iteration/output tail) for
    /// `node_id` within `spec_id` — for a node the user has navigated to in
    /// the graph, which may differ from `graph_live_state`'s auto-detected
    /// current node.
    pub(crate) fn graph_node_run_info(
        &self,
        spec_id: &str,
        node_id: &str,
    ) -> graph_live_state::NodeRunInfo {
        graph_live_state::resolve_node_run_info(&self.db, spec_id, node_id)
    }

    pub fn delete_selected_project(&mut self) -> Result<()> {
        let Some(hash) = self.selected_project().map(|p| p.hash.clone()) else {
            return Ok(());
        };
        self.db.delete_project(&hash)?;
        self.refresh_projects()?;
        self.refresh_graphs()?;
        self.refresh_project_graph().ok();
        self.refresh_rag_state()?;
        Ok(())
    }

    /// Archive the graph currently selected in the main (non-archived) view.
    /// A no-op (not an error) when nothing is selected, the graph is already
    /// archived, or it's still `running` (archiving is for work that's
    /// finished with — pause it first).
    pub fn archive_selected_graph(&mut self) -> Result<()> {
        let Some(lp) = self.selected_graph() else {
            return Ok(());
        };
        self.db.archive_graph(&lp.id)?;
        self.refresh_graphs()?;
        self.refresh_projects()?;
        self.refresh_rag_state()?;
        Ok(())
    }

    /// Restore the graph currently selected in the archived view back to the
    /// main list. A no-op when nothing is selected.
    pub fn restore_selected_archived_graph(&mut self) -> Result<()> {
        let Some(lp) = self.selected_graph() else {
            return Ok(());
        };
        self.db.restore_graph(&lp.id)?;
        self.refresh_graphs()?;
        self.refresh_projects()?;
        self.refresh_rag_state()?;
        Ok(())
    }

    /// Permanently delete the graph currently selected in the archived view —
    /// the deliberate, separate act this spec keeps behind the archive: it
    /// destroys the graph's row and, via `ON DELETE CASCADE`, its specs and
    /// full run history. A no-op when nothing is selected.
    pub fn permanent_delete_selected_archived_graph(&mut self) -> Result<()> {
        let Some(lp) = self.selected_graph() else {
            return Ok(());
        };
        self.db.delete_graph(&lp.id)?;
        self.refresh_graphs()?;
        self.refresh_projects()?;
        self.refresh_rag_state()?;
        Ok(())
    }

    /// Toggle the Graphs sidebar section between the main list and the
    /// archive. Refreshes immediately so the archived list is populated the
    /// moment it becomes visible (`refresh_graphs` only loads
    /// `archived_graphs` while this flag is set).
    pub fn toggle_graph_archive_view(&mut self) {
        self.graph_view_archived = !self.graph_view_archived;
        let _ = self.refresh_graphs();
    }

    pub fn delete_selected_knowledge(&mut self) -> Result<()> {
        let Some(node) = self.project_knowledge.get(self.selected_knowledge) else {
            return Ok(());
        };
        let id = node.id.clone();
        self.db.delete_intelligence_node(&id)?;
        self.refresh_project_knowledge()?;
        Ok(())
    }

    pub fn filtered_knowledge_indices(&self) -> Vec<usize> {
        let query = self.knowledge_filter.trim().to_lowercase();
        self.project_knowledge
            .iter()
            .enumerate()
            .filter(|(_, node)| {
                if query.is_empty() {
                    return true;
                }

                node.title.to_lowercase().contains(&query)
                    || node.body.to_lowercase().contains(&query)
                    || node.kind.to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn append_knowledge_filter(&mut self, value: char) {
        self.knowledge_filter.push(value);
        self.normalize_selected_knowledge();
    }

    pub fn pop_knowledge_filter(&mut self) {
        self.knowledge_filter.pop();
        self.normalize_selected_knowledge();
    }

    pub fn clear_knowledge_filter(&mut self) {
        self.knowledge_filter.clear();
        self.normalize_selected_knowledge();
    }

    pub fn enter_knowledge_filter_mode(&mut self) {
        self.knowledge_filter_mode = true;
    }

    pub fn exit_knowledge_filter_mode(&mut self) {
        self.knowledge_filter_mode = false;
    }

    fn normalize_selected_knowledge(&mut self) {
        let filtered = self.filtered_knowledge_indices();
        if filtered.is_empty() {
            self.selected_knowledge = 0;
            return;
        }

        if filtered.contains(&self.selected_knowledge) {
            return;
        }

        self.selected_knowledge = filtered[0];
    }

    /// Entry point for arrow-down/up from the `Home` screen: focus the
    /// nearest navigable edge of the sidebar ring (RAG → Live → Automation →
    /// Knowledge). Distinct from in-sidebar arrow navigation, which never
    /// crosses tabs once focus is inside one.
    pub fn activate_playground(&mut self) {
        self.playground_active = true;
        self.reset_playground_state();
        // Personal RAG is global — no project_hash filter.
        self.playground_project_hash = None;
    }

    pub fn deactivate_playground(&mut self) {
        self.playground_active = false;
        self.reset_playground_state();
    }

    fn reset_playground_state(&mut self) {
        self.playground_query.clear();
        self.playground_results.clear();
        self.playground_selected = 0;
        self.playground_search_pending = false;
        self.playground_last_executed_query.clear();
        self.playground_detail_mode = false;
        self.playground_scroll = 0;
    }

    pub fn toggle_rag_pause(&mut self) {
        let new_val = !self.rag_paused;
        let _ = self
            .db
            .set_state("rag_paused", if new_val { "1" } else { "0" });
        self.rag_paused = new_val;
    }

    pub fn cycle_graph_spec(&mut self, forward: bool) {
        let Some(details) = self.graph_details.as_ref() else {
            return;
        };
        if details.specs.is_empty() {
            return;
        }

        self.graph_selected_spec = crate::tui::selection::move_index(
            self.graph_selected_spec,
            details.specs.len(),
            forward,
        );
        self.graph_selected_node = 0;
        self.refresh_graph_runs_for_selected_spec();
        self.select_default_graph_node_if_needed(true);
        self.reset_log_scroll();
    }

    pub fn selected_playground_chunk(&self) -> Option<&crate::rag::vector_store::SearchResult> {
        self.playground_results.get(self.playground_selected)
    }

    pub fn toggle_activity_panel(&mut self) {
        if self.sidebar_layer == SidebarLayer::Knowledge {
            return;
        }

        let Some(workdir) = self.selected_activity_workdir().map(str::to_owned) else {
            return;
        };
        let panel_rendered = self
            .activity_panel_layout_width(self.term_width, self.activity_panel_state().is_some())
            > 0;

        if self.hidden_activity_workdirs.remove(&workdir) {
            self.forced_activity_workdirs.insert(workdir);
            self.sync_scroll_offset = 0;
            return;
        }

        // Nueva lógica: siempre permite togglear el panel de actividad con F3,
        // aunque no haya actividad previa en el workdir seleccionado.
        if panel_rendered || self.forced_activity_workdirs.contains(&workdir) {
            self.forced_activity_workdirs.remove(&workdir);
            self.hidden_activity_workdirs.insert(workdir);
        } else {
            self.forced_activity_workdirs.insert(workdir);
        }
        self.sync_scroll_offset = 0;
    }

    fn live_agent_for_entry(&self, entry: &AgentEntry) -> Option<&InteractiveAgent> {
        match entry {
            AgentEntry::Interactive(idx) => self.interactive_agents.get(*idx),
            AgentEntry::Terminal(idx) => self.terminal_agents.get(*idx),
            AgentEntry::Agent(_)
            | AgentEntry::Corrupt(_)
            | AgentEntry::Group(_)
            | AgentEntry::Orphaned(_) => None,
        }
    }

    fn selected_live_agent(&self) -> Option<&InteractiveAgent> {
        self.selected_agent()
            .and_then(|entry| self.live_agent_for_entry(entry))
    }

    /// Return the working directory of the currently selected agent,
    /// or the parent of the data directory as a fallback.
    pub fn current_workdir(&self) -> PathBuf {
        if let Some(workdir) = self.workdir_for_projects_mode() {
            return workdir;
        }
        if let Some(workdir) = self.workdir_for_selected_agent() {
            return workdir;
        }
        self.data_dir
            .parent()
            .unwrap_or(&self.data_dir)
            .to_path_buf()
    }

    fn workdir_for_projects_mode(&self) -> Option<PathBuf> {
        if self.sidebar_layer != SidebarLayer::Knowledge {
            return None;
        }
        self.selected_project().map(|p| PathBuf::from(&p.path))
    }

    fn workdir_for_selected_agent(&self) -> Option<PathBuf> {
        self.selected_live_agent()
            .map(|agent| PathBuf::from(&agent.working_dir))
    }

    /// Return a unique key for the current prompt-builder session.
    /// Uses the agent/session ID when available, falls back to workdir path.
    pub fn current_prompt_session_key(&self) -> String {
        if let Some(key) = self.prompt_session_key_for_selected_agent() {
            return key;
        }
        if let Some(key) = self.prompt_session_key_for_selected_project() {
            return key;
        }
        format!("workdir:{}", self.current_workdir().display())
    }

    fn prompt_session_key_for_selected_agent(&self) -> Option<String> {
        let entry = self.selected_agent()?;
        match entry {
            types::AgentEntry::Interactive(idx) => {
                let agent = self.interactive_agents.get(*idx)?;
                Some(format!("interactive:{}", agent.id))
            }
            types::AgentEntry::Terminal(idx) => {
                let agent = self.terminal_agents.get(*idx)?;
                Some(format!("terminal:{}", agent.id))
            }
            types::AgentEntry::Agent(a) => Some(format!("agent:{}", a.id)),
            types::AgentEntry::Corrupt(_)
            | types::AgentEntry::Group(_)
            | types::AgentEntry::Orphaned(_) => None,
        }
    }

    fn prompt_session_key_for_selected_project(&self) -> Option<String> {
        if self.sidebar_layer != SidebarLayer::Knowledge {
            return None;
        }
        let project = self.selected_project()?;
        Some(format!("project:{}", project.path))
    }

    pub fn focused_agent_name(&self) -> String {
        self.selected_live_agent()
            .map(|agent| agent.name.clone())
            .unwrap_or_default()
    }

    pub fn selected_id(&self) -> String {
        self.selected_agent()
            .map(|a| a.id(self).to_string())
            .unwrap_or_else(|| "—".to_string())
    }

    /// Record a CLI launch in usage stats and persist to disk.
    pub fn record_cli_usage(&mut self, cli_name: &str) {
        self.cli_usage.record(cli_name);
        let _ =
            dirs::home_dir().and_then(|h| self.cli_usage.save(&h.join(".canopy")).ok().map(|_| ()));
    }

    pub fn toggle_enable(&self) -> Result<()> {
        let Some(AgentEntry::Agent(agent)) = self.agents.get(self.selected) else {
            return Ok(());
        };

        self.db.update_agent_enabled(&agent.id, !agent.enabled)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::process_matches_cli;
    use super::{
        adaptive_change_score, adaptive_poll_interval_ms, blend_optional_f32, blend_optional_f64,
        build_resumed_session_args, calculate_log_hash, lerp_f32, lerp_u64, log_contains_error,
        log_contains_spawn, log_contains_success, process_is_alive, process_outlives_grace,
        resume_decision, sample_from, should_resume_session, ResumeDecision, SystemSample,
    };
    use crate::db::session::InteractiveSession;
    use crate::db::Database;
    use crate::domain::graphs::{GraphSpecStatus, GraphStatus};
    use crate::tui::app::graph_live_state::{GraphLiveState, SpecQueueEntry};
    use crate::tui::app::types::{
        AgentEntry, App, AutomationKind, Focus, GraphLiveFocus, ProjectTab, SidebarLayer,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    #[test]
    fn reconcile_bridge_sessions_reaps_dead_pid_but_leaves_live_bridge_active() {
        let db = test_db();
        db.insert_interactive_session(
            "dead-bridge",
            "standalone",
            "bridge",
            "/tmp",
            Some("canopy bridge"),
            Some(999_999_999),
            "bridge",
            None,
        )
        .unwrap();
        db.insert_interactive_session(
            "live-bridge",
            "standalone",
            "bridge",
            "/tmp",
            Some("canopy bridge"),
            Some(std::process::id() as i64),
            "bridge",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.reconcile_bridge_sessions();

        let still_active = db.get_active_sessions_by_type("bridge").unwrap();
        assert_eq!(still_active.len(), 1);
        assert_eq!(still_active[0].id, "live-bridge");
    }

    #[test]
    fn test_yolo_mode_preservation_in_session_relaunch() {
        let session = InteractiveSession {
            id: "test-session".to_string(),
            name: "test-session".to_string(),
            cli: "opencode".to_string(),
            working_dir: "/tmp".to_string(),
            args: Some("--tui --yolo".to_string()),
            started_at: "2023-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        };

        assert!(build_resumed_session_args(
            session.args.as_deref(),
            None,
            None,
            None,
            Some("--yolo")
        )
        .as_deref()
        .is_some_and(|args| args.contains("--yolo")));
    }

    #[test]
    fn test_yolo_flag_not_duplicated_when_falling_back_to_original_args() {
        let session = InteractiveSession {
            id: "test-session".to_string(),
            name: "test-session".to_string(),
            cli: "opencode".to_string(),
            working_dir: "/tmp".to_string(),
            args: Some("--tui --yolo".to_string()),
            started_at: "2023-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        };

        let args =
            build_resumed_session_args(session.args.as_deref(), None, None, None, Some("--yolo"))
                .unwrap();
        assert_eq!(args.matches("--yolo").count(), 1);
    }

    #[test]
    fn test_original_resume_args_preserved_over_reconstructed_args() {
        let session = InteractiveSession {
            id: "test-session".to_string(),
            name: "test-session".to_string(),
            cli: "opencode".to_string(),
            working_dir: "/tmp".to_string(),
            args: Some("--session abc123 --yolo".to_string()),
            started_at: "2023-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        };

        let args = build_resumed_session_args(
            session.args.as_deref(),
            Some("--chat"),
            Some("-c"),
            Some("--session"),
            Some("--yolo"),
        )
        .unwrap();
        assert!(args.contains("--session abc123"));
        assert!(args.contains("--yolo"));
        assert!(!args.contains("-c"));
    }

    #[test]
    fn test_rebuilds_resume_args_for_fresh_session() {
        let session = InteractiveSession {
            id: "test-session".to_string(),
            name: "test-session".to_string(),
            cli: "copilot".to_string(),
            working_dir: "/tmp".to_string(),
            args: None,
            started_at: "2023-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        };

        let args = build_resumed_session_args(
            session.args.as_deref(),
            None,
            Some("--continue"),
            None,
            Some("--yolo"),
        )
        .unwrap();
        assert!(args.contains("--continue"));
        assert!(!args.contains("--yolo"));
    }

    #[test]
    fn test_appends_resume_args_to_original_interactive_command() {
        let session = InteractiveSession {
            id: "test-session".to_string(),
            name: "test-session".to_string(),
            cli: "kiro".to_string(),
            working_dir: "/tmp".to_string(),
            args: Some("chat --trust-all-tools".to_string()),
            started_at: "2023-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        };

        let args = build_resumed_session_args(
            session.args.as_deref(),
            Some("chat"),
            Some("--resume-picker"),
            None,
            Some("--trust-all-tools"),
        )
        .unwrap();
        assert!(args.contains("chat"));
        assert!(args.contains("--resume-picker"));
        assert_eq!(args.matches("--trust-all-tools").count(), 1);
    }

    #[test]
    fn test_process_is_alive_for_own_pid() {
        assert!(process_is_alive(std::process::id() as i64));
    }

    #[test]
    fn test_process_is_alive_false_for_implausible_pid() {
        assert!(!process_is_alive(999_999_999));
    }

    #[test]
    fn test_should_resume_session_with_no_pid_always_resumes() {
        let current = crate::system::boot_id();
        assert!(should_resume_session(
            None,
            current.as_deref(),
            current.as_deref()
        ));
    }

    #[test]
    fn test_should_resume_session_skips_when_owner_process_is_alive() {
        let current = crate::system::boot_id();
        assert!(!should_resume_session(
            Some(std::process::id() as i64),
            current.as_deref(),
            current.as_deref()
        ));
    }

    #[test]
    fn test_should_resume_session_resumes_when_pid_is_gone() {
        let current = crate::system::boot_id();
        assert!(should_resume_session(
            Some(999_999_999),
            current.as_deref(),
            current.as_deref()
        ));
    }

    #[test]
    fn test_should_resume_session_resumes_on_boot_id_mismatch_even_with_live_pid() {
        let current = crate::system::boot_id();
        // Stored boot_id differs from current → PID is meaningless, always resume.
        assert!(should_resume_session(
            Some(std::process::id() as i64),
            Some("old-boot-id-from-previous-reboot"),
            current.as_deref(),
        ));
    }

    #[test]
    fn test_should_resume_session_resumes_when_stored_boot_id_is_null() {
        let current = crate::system::boot_id();
        // Legacy row with NULL boot_id → always resume.
        assert!(should_resume_session(
            Some(std::process::id() as i64),
            None,
            current.as_deref(),
        ));
    }

    #[test]
    fn test_should_resume_session_resumes_when_current_boot_id_is_none() {
        // Non-Linux host where boot_id can't be read → always resume.
        assert!(should_resume_session(
            Some(std::process::id() as i64),
            Some("some-stored-boot-id"),
            None,
        ));
    }

    #[test]
    fn test_resume_decision_skips_when_pid_alive_same_boot_and_outlives_grace() {
        // The only genuine conflict: a live PID on this boot that survives
        // the full grace window — some other process still holds the lock.
        assert_eq!(
            resume_decision(true, true),
            ResumeDecision::SkipHeldByLiveProcess
        );
    }

    #[test]
    fn test_resume_decision_resumes_when_pid_alive_same_boot_but_grace_expires() {
        // Live PID, same boot, but it didn't survive the grace window: mid-death
        // or recycled, not a real holder.
        assert_eq!(resume_decision(true, false), ResumeDecision::Resume);
    }

    #[test]
    fn test_resume_decision_resumes_when_pid_not_alive_same_boot() {
        assert_eq!(resume_decision(false, false), ResumeDecision::Resume);
    }

    #[test]
    fn test_resume_decision_resumes_when_pid_not_alive_same_boot_even_if_grace_flag_set() {
        // outlives_grace is meaningless when the PID isn't the live-same-boot
        // case in the first place; pid_alive_same_boot alone must gate it.
        assert_eq!(resume_decision(false, true), ResumeDecision::Resume);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_process_matches_cli_detects_recycled_pids() {
        // Our own PID is alive but its comm is the test binary, not "claude" —
        // exactly the recycled-PID case that must NOT orphan a session.
        let own_pid = std::process::id() as i64;
        assert!(!process_matches_cli(own_pid, "claude"));

        // And it does match its own real comm.
        let own_comm = std::fs::read_to_string(format!("/proc/{own_pid}/comm"))
            .expect("read own comm")
            .trim()
            .to_string();
        assert!(process_matches_cli(own_pid, &own_comm));
    }

    #[test]
    fn test_process_outlives_grace_false_for_dead_or_recycled_pids() {
        // A PID nothing owns: resume immediately, no grace wait.
        assert!(!process_outlives_grace(-1, "claude"));
        // A live PID whose comm is another binary (recycled): also no wait.
        assert!(!process_outlives_grace(std::process::id() as i64, "claude"));
    }

    #[test]
    fn test_process_outlives_grace_waits_out_a_dying_process() {
        // A child that exits shortly after we check: the grace graph must
        // observe the death and report "no conflict" instead of orphaning.
        // The child is reaped on a side thread — an unreaped zombie would
        // still answer kill(pid, 0). (In production the contended PID never
        // belongs to a child of the new TUI, so there is no zombie window.)
        let mut child = std::process::Command::new("sleep")
            .arg("0.3")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i64;
        let reaper = std::thread::spawn(move || {
            let _ = child.wait();
        });
        assert!(!process_outlives_grace(pid, "sleep"));
        reaper.join().expect("join reaper");
    }

    // ── Sidebar: graphs/backlog/history sections ─────────────────────

    fn make_project(hash: &str, path: &str) -> crate::domain::project::Project {
        crate::domain::project::Project {
            hash: hash.to_string(),
            path: path.to_string(),
            name: hash.to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 0,
        }
    }

    fn make_graph(
        id: &str,
        name: &str,
        status: crate::domain::graphs::GraphStatus,
    ) -> crate::domain::graphs::Graph {
        crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        }
    }

    fn make_backlog_spec(
        id: &str,
        name: &str,
        workdir: Option<&str>,
    ) -> crate::domain::graphs::GraphSpec {
        crate::domain::graphs::GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: name.to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: workdir.map(str::to_string),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        }
    }

    #[test]
    fn sidebar_graphs_orders_by_recency_across_all_statuses() {
        use crate::domain::graphs::GraphStatus;

        let db = test_db();
        let base = chrono::Utc::now() - chrono::Duration::hours(10);

        let mut draft = make_graph("l-draft", "Draft Graph", GraphStatus::Draft);
        draft.created_at = base;
        db.insert_graph(&draft).unwrap();

        let mut failed = make_graph("l-failed", "Failed Graph", GraphStatus::Failed);
        failed.created_at = base + chrono::Duration::minutes(10);
        db.insert_graph(&failed).unwrap();

        let mut done = make_graph("l-done", "Done Graph", GraphStatus::Completed);
        done.created_at = base + chrono::Duration::minutes(20);
        db.insert_graph(&done).unwrap();

        let mut paused = make_graph("l-paused", "Paused Graph", GraphStatus::Paused);
        paused.created_at = base + chrono::Duration::minutes(30);
        db.insert_graph(&paused).unwrap();

        let mut running = make_graph("l-running", "Running Graph", GraphStatus::Running);
        running.created_at = base + chrono::Duration::minutes(40);
        db.insert_graph(&running).unwrap();

        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let ids: Vec<&str> = app
            .sidebar_graphs()
            .iter()
            .map(|lp| lp.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["l-running", "l-paused", "l-done", "l-failed", "l-draft"],
            "every graph must be listed regardless of status, ordered by last \
             activity (created_at, since none of these have run) most recent first"
        );
    }

    #[test]
    fn refresh_backlog_specs_filters_by_selected_project_workdir() {
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        db.upsert_project(&make_project("hash1", "/tmp/proj1"))
            .unwrap();
        db.insert_graph_spec(&make_backlog_spec("spec-a", "Spec A", Some("/tmp/proj0")))
            .unwrap();
        db.insert_graph_spec(&make_backlog_spec("spec-b", "Spec B", Some("/tmp/proj1")))
            .unwrap();
        db.insert_graph_spec(&make_backlog_spec("spec-c", "Spec C", None))
            .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // Projects are ranked by recency with second-resolution timestamps, so
        // two upserts may land in either order: select by hash, never by index.
        let index_of = |app: &App, hash: &str| {
            app.projects
                .iter()
                .position(|p| p.hash == hash)
                .expect("project listed")
        };

        app.selected_project = index_of(&app, "hash0");
        app.refresh_backlog_specs().expect("refresh backlog");
        assert_eq!(
            app.backlog_specs
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
            vec!["Spec A".to_string()],
            "backlog should be tag-filtered to the selected project's workdir"
        );

        app.selected_project = index_of(&app, "hash1");
        app.refresh_backlog_specs().expect("refresh backlog");
        assert_eq!(
            app.backlog_specs
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>(),
            vec!["Spec B".to_string()]
        );
    }

    #[test]
    fn entering_a_project_defaults_to_overview_and_history_lazily_loads() {
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.project_focus.is_none(), "starts on Preview, not Focus");

        app.enter_project_focus(ProjectTab::Overview);
        assert_eq!(app.project_focus, Some(ProjectTab::Overview));

        app.open_project_tab(ProjectTab::History);
        assert_eq!(app.project_focus, Some(ProjectTab::History));
        assert!(
            app.project_history_cache.contains_key("hash0"),
            "History tab lazily loads persisted data on first show"
        );

        app.exit_project_focus();
        assert!(app.project_focus.is_none());
    }

    #[test]
    fn cycle_project_tab_wraps_through_all_four_tabs() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.enter_project_focus(ProjectTab::Overview);

        app.cycle_project_tab(true);
        assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
        app.cycle_project_tab(true);
        assert_eq!(app.project_focus, Some(ProjectTab::Knowledge));
        app.cycle_project_tab(true);
        assert_eq!(app.project_focus, Some(ProjectTab::History));
        app.cycle_project_tab(true);
        assert_eq!(app.project_focus, Some(ProjectTab::Overview));

        app.cycle_project_tab(false);
        assert_eq!(app.project_focus, Some(ProjectTab::History));
    }

    #[test]
    fn select_prev_on_first_live_item_does_not_select_automation() {
        // Regression: standing on the first Live item and pressing "up"
        // used to fall through to the last Automation entry (the old
        // flat-ring `cross_layer` behavior, back when background agents
        // lived in Live before tabs existed). Arrows must now stay inside
        // the active tab — with a populated Automation tab present, "up"
        // must not land there.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![
            AgentEntry::Group(0),
            AgentEntry::Group(1),
            AgentEntry::Agent(bg_agent("bg-1")),
        ];
        app.sidebar_layer = SidebarLayer::Live;
        app.automation_kind = AutomationKind::Agent;
        app.selected = 0;

        app.select_prev();

        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_ne!(
            app.selected, 2,
            "must not land on the Automation agent entry"
        );
    }

    #[test]
    fn navigate_live_wraps_at_both_ends() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 0;

        // Backward off the first item wraps to the last (no RAG activity to
        // divert to).
        app.select_prev();
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_eq!(app.selected, 1);

        // Forward off the last item wraps back to the first.
        app.select_next();
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn navigate_automation_wraps_at_both_ends() {
        use crate::domain::graphs::GraphStatus;

        let db = test_db();
        db.insert_graph(&make_graph(
            "l-active",
            "Active Graph",
            GraphStatus::Running,
        ))
        .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = AutomationKind::Agent;
        app.selected = 0;

        // Backward off the first entry (the agent) wraps to the last (the
        // graph) instead of leaving Automation.
        app.select_prev();
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        assert_eq!(app.automation_kind, AutomationKind::Graph);
        assert_eq!(app.selected_graph_id.as_deref(), Some("l-active"));

        // Forward off the last entry (the graph) wraps back to the first
        // (the agent).
        app.select_next();
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        assert_eq!(app.automation_kind, AutomationKind::Agent);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn navigate_projects_wraps_at_both_ends() {
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        db.upsert_project(&make_project("hash1", "/tmp/proj1"))
            .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.selected_project = 0;

        // Backward off the first project wraps to the last.
        app.select_prev();
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
        assert_eq!(app.selected_project, 1);

        // Forward off the last project wraps back to the first.
        app.select_next();
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
        assert_eq!(app.selected_project, 0);
    }

    #[test]
    fn rag_reachable_upward_from_live_first_item_returns_to_live() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 0;
        app.rag_info = crate::db::project::RagInfoSummary {
            total_chunks: 5,
            ..Default::default()
        };

        app.select_prev();
        assert!(app.agents_rag_focused);
        assert_eq!(
            app.sidebar_layer,
            SidebarLayer::Live,
            "entering RAG focus must not change the active tab"
        );

        app.select_next();
        assert!(!app.agents_rag_focused);
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_eq!(
            app.selected, 0,
            "leaving RAG lands on the first item of the tab it came from"
        );
    }

    #[test]
    fn rag_reachable_upward_from_automation_first_item_returns_to_automation() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = AutomationKind::Agent;
        app.selected = 0;
        app.rag_info = crate::db::project::RagInfoSummary {
            total_chunks: 5,
            ..Default::default()
        };

        app.select_prev();
        assert!(app.agents_rag_focused);
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);

        app.select_next();
        assert!(!app.agents_rag_focused);
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        assert_eq!(app.automation_kind, AutomationKind::Agent);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn rag_reachable_upward_from_knowledge_first_item_returns_to_knowledge() {
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.selected_project = 0;
        app.rag_info = crate::db::project::RagInfoSummary {
            total_chunks: 5,
            ..Default::default()
        };

        app.select_prev();
        assert!(app.agents_rag_focused);
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);

        app.select_next();
        assert!(!app.agents_rag_focused);
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
        assert_eq!(app.selected_project, 0);
    }

    #[test]
    fn switch_sidebar_tab_lands_on_an_empty_tab_instead_of_refusing() {
        // Unlike `cycle_sidebar_layer` (which skips empty layers so
        // keyboard cycling never lands somewhere with nothing to select), a
        // deliberate mouse click on a tab must always switch to it — even
        // Automation with nothing running, so its empty state is reachable.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);

        app.switch_sidebar_tab(SidebarLayer::Automation);

        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
    }

    #[test]
    fn switch_sidebar_tab_selects_first_item_when_the_tab_has_content() {
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.switch_sidebar_tab(SidebarLayer::Knowledge);

        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
        assert_eq!(app.selected_project, 0);
    }

    // ── Pure helper tests ────────────────────────────────────────

    #[test]
    fn calculate_log_hash_empty_string() {
        assert_eq!(calculate_log_hash(""), 0);
    }

    #[test]
    fn calculate_log_hash_deterministic() {
        let h1 = calculate_log_hash("hello world");
        let h2 = calculate_log_hash("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn calculate_log_hash_different_inputs() {
        let h1 = calculate_log_hash("hello");
        let h2 = calculate_log_hash("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn log_contains_error_positive() {
        assert!(log_contains_error("ERROR: something broke"));
        assert!(log_contains_error("FAILED to connect"));
        assert!(log_contains_error("EXCEPTION thrown"));
        assert!(log_contains_error("PANIC in module"));
        assert!(log_contains_error("SEGFAULT detected"));
        assert!(log_contains_error("TIMED OUT after 30s"));
        assert!(log_contains_error("CONNECTION REFUSED"));
        assert!(log_contains_error("PERMISSION DENIED"));
        assert!(log_contains_error("HALTED unexpectedly"));
        assert!(log_contains_error("PROBLEMA detectado"));
        assert!(log_contains_error("FALLO en el sistema"));
        assert!(log_contains_error("FALLANDO test"));
    }

    #[test]
    fn log_contains_error_negative() {
        assert!(!log_contains_error("everything is fine"));
        assert!(!log_contains_error("SUCCESS all done"));
        assert!(!log_contains_error(""));
    }

    #[test]
    fn log_contains_success_positive() {
        assert!(log_contains_success("SUCCESS"));
        assert!(log_contains_success("ALL TESTS PASSED"));
        assert!(log_contains_success("BUILD SUCCEEDED"));
        assert!(log_contains_success("FINISHED task"));
        assert!(log_contains_success("COMPLETED"));
        assert!(log_contains_success("DONE."));
        assert!(log_contains_success("STABILIZED"));
        assert!(log_contains_success("READY to deploy"));
        assert!(log_contains_success("CONVERGED"));
        assert!(log_contains_success("DEPLOYED to prod"));
        assert!(log_contains_success("EXCELENTE resultado"));
        assert!(log_contains_success("COMPLETADO"));
        assert!(log_contains_success("HECHO"));
        assert!(log_contains_success("LISTO"));
        assert!(log_contains_success("TERMINADO"));
    }

    #[test]
    fn log_contains_success_negative() {
        assert!(!log_contains_success("ERROR: failed"));
        assert!(!log_contains_success("running tests..."));
        assert!(!log_contains_success(""));
    }

    #[test]
    fn log_contains_spawn_positive() {
        assert!(log_contains_spawn("SPAWNING agent"));
        assert!(log_contains_spawn("STARTING UP server"));
        assert!(log_contains_spawn("BOOTSTRAPPING cluster"));
        assert!(log_contains_spawn("INITIALIZING module"));
    }

    #[test]
    fn log_contains_spawn_negative() {
        assert!(!log_contains_spawn("agent stopped"));
        assert!(!log_contains_spawn("DONE"));
        assert!(!log_contains_spawn(""));
    }

    #[test]
    fn lerp_f32_midpoint() {
        assert!((lerp_f32(0.0, 10.0, 0.5) - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lerp_f32_endpoints() {
        assert!((lerp_f32(0.0, 10.0, 0.0) - 0.0).abs() < f32::EPSILON);
        assert!((lerp_f32(0.0, 10.0, 1.0) - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lerp_f32_negative() {
        assert!((lerp_f32(10.0, 0.0, 0.5) - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn lerp_u64_midpoint() {
        assert_eq!(lerp_u64(0, 100, 0.5), 50);
    }

    #[test]
    fn lerp_u64_endpoints() {
        assert_eq!(lerp_u64(0, 100, 0.0), 0);
        assert_eq!(lerp_u64(0, 100, 1.0), 100);
    }

    #[test]
    fn blend_optional_f32_both_present() {
        assert_eq!(blend_optional_f32(Some(0.0), Some(10.0), 0.5), Some(5.0));
    }

    #[test]
    fn blend_optional_f32_first_none() {
        assert_eq!(blend_optional_f32(None, Some(10.0), 0.5), Some(10.0));
    }

    #[test]
    fn blend_optional_f32_second_none() {
        assert_eq!(blend_optional_f32(Some(0.0), None, 0.5), None);
    }

    #[test]
    fn blend_optional_f32_both_none() {
        assert_eq!(blend_optional_f32(None, None, 0.5), None);
    }

    #[test]
    fn blend_optional_f64_both_present() {
        let result = blend_optional_f64(Some(0.0), Some(10.0), 0.5);
        assert!((result.unwrap() - 5.0).abs() < 0.01);
    }

    #[test]
    fn blend_optional_f64_first_none() {
        assert_eq!(blend_optional_f64(None, Some(10.0), 0.5), Some(10.0));
    }

    #[test]
    fn adaptive_change_score_identical() {
        let s = SystemSample {
            cpu_usage: 50.0,
            mem_pct: 60.0,
            load: 1.0,
            cpu_temp: 40.0,
            gpu_usage: 30.0,
            gpu_temp: 50.0,
        };
        assert!((adaptive_change_score(s, s) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn adaptive_change_score_max_change() {
        let prev = SystemSample {
            cpu_usage: 0.0,
            mem_pct: 0.0,
            load: 0.0,
            cpu_temp: 0.0,
            gpu_usage: 0.0,
            gpu_temp: 0.0,
        };
        let next = SystemSample {
            cpu_usage: 100.0,
            mem_pct: 100.0,
            load: 2.0,
            cpu_temp: 20.0,
            gpu_usage: 100.0,
            gpu_temp: 20.0,
        };
        let score = adaptive_change_score(prev, next);
        assert!(score > 0.8);
        assert!(score <= 1.0);
    }

    #[test]
    fn adaptive_poll_interval_ms_fast_when_high_change() {
        let ms = adaptive_poll_interval_ms(1.0);
        assert!(ms <= 1000);
    }

    #[test]
    fn adaptive_poll_interval_ms_slow_when_no_change() {
        let ms = adaptive_poll_interval_ms(0.0);
        assert!(ms >= 2500);
    }

    #[test]
    fn adaptive_poll_interval_ms_clamped() {
        let ms_low = adaptive_poll_interval_ms(-1.0);
        let ms_high = adaptive_poll_interval_ms(2.0);
        assert!(ms_low >= 500);
        assert!(ms_high <= 3000);
    }

    #[test]
    fn sample_from_basic_conversion() {
        let info = crate::system::SystemInfo {
            cpu_usage: 42.5,
            memory_used: 4_000_000_000,
            memory_total: 8_000_000_000,
            load_average: Some(1.5),
            cpu_temperature: Some(55.0),
            gpu_info: Some(crate::system::GpuInfo {
                name: "RTX 4090".to_string(),
                vendor: "NVIDIA".to_string(),
                usage: Some(70.0),
                temperature: Some(65.0),
                vram_used: Some(8_000_000_000),
                vram_total: Some(24_000_000_000),
                power_watts: Some(300.0),
                power_limit_watts: Some(450.0),
            }),
            ..crate::system::SystemInfo::default()
        };
        let sample = sample_from(&info);
        assert!((sample.cpu_usage - 42.5).abs() < f32::EPSILON);
        assert!((sample.mem_pct - 50.0).abs() < 0.1);
        assert!((sample.load - 1.5).abs() < f32::EPSILON);
        assert!((sample.cpu_temp - 55.0).abs() < f32::EPSILON);
        assert!((sample.gpu_usage - 70.0).abs() < f32::EPSILON);
        assert!((sample.gpu_temp - 65.0).abs() < f32::EPSILON);
    }

    #[test]
    fn sample_from_zero_memory_total() {
        let info = crate::system::SystemInfo {
            memory_used: 1000,
            memory_total: 0,
            ..crate::system::SystemInfo::default()
        };
        let sample = sample_from(&info);
        assert!((sample.mem_pct).abs() < f32::EPSILON);
    }

    #[test]
    fn sample_from_no_gpu() {
        let info = crate::system::SystemInfo {
            gpu_info: None,
            ..crate::system::SystemInfo::default()
        };
        let sample = sample_from(&info);
        assert!((sample.gpu_usage).abs() < f32::EPSILON);
        assert!((sample.gpu_temp).abs() < f32::EPSILON);
    }

    #[test]
    fn sidebar_tab_index_returns_correct_index() {
        assert_eq!(App::sidebar_tab_index(SidebarLayer::Live), 0);
        assert_eq!(App::sidebar_tab_index(SidebarLayer::Automation), 1);
        assert_eq!(App::sidebar_tab_index(SidebarLayer::Knowledge), 2);
    }

    #[test]
    fn update_prompt_config_on_object() {
        let config = serde_json::json!({"platform": "claude", "model": "sonnet"});
        let result = App::update_prompt_config(&config, "new prompt here");
        assert_eq!(result["prompt_template"], "new prompt here");
        assert_eq!(result["platform"], "claude");
        assert_eq!(result["model"], "sonnet");
    }

    #[test]
    fn update_prompt_config_on_non_object() {
        let config = serde_json::json!("just a string");
        let result = App::update_prompt_config(&config, "prompt text");
        assert_eq!(result["prompt_template"], "prompt text");
    }

    #[test]
    fn update_prompt_config_preserves_existing_prompt_template() {
        let config = serde_json::json!({"prompt_template": "old prompt"});
        let result = App::update_prompt_config(&config, "replaced");
        assert_eq!(result["prompt_template"], "replaced");
    }

    #[test]
    fn update_prompt_config_empty_object() {
        let config = serde_json::json!({});
        let result = App::update_prompt_config(&config, "test");
        assert_eq!(result["prompt_template"], "test");
    }

    #[test]
    fn terminal_selection_normalized_ordering() {
        let sel = crate::tui::app::types::TerminalSelection {
            agent: (true, 0),
            start: (5, 10),
            end: (2, 3),
            dragging: false,
        };
        let (s, e) = sel.normalized();
        assert_eq!(s, (2, 3));
        assert_eq!(e, (5, 10));
    }

    #[test]
    fn terminal_selection_normalized_already_ordered() {
        let sel = crate::tui::app::types::TerminalSelection {
            agent: (false, 1),
            start: (1, 2),
            end: (3, 4),
            dragging: false,
        };
        let (s, e) = sel.normalized();
        assert_eq!(s, (1, 2));
        assert_eq!(e, (3, 4));
    }

    #[test]
    fn terminal_selection_normalized_equal_endpoints() {
        let sel = crate::tui::app::types::TerminalSelection {
            agent: (true, 0),
            start: (2, 3),
            end: (2, 3),
            dragging: false,
        };
        let (s, e) = sel.normalized();
        assert_eq!(s, (2, 3));
        assert_eq!(e, (2, 3));
    }

    // ── GraphEditorDialog tests ──────────────────────────────────

    #[test]
    fn graph_editor_dialog_new_sets_cursor_at_end() {
        let dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "title".into(),
            "help".into(),
            "hello world".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        assert_eq!(dialog.cursor, 11); // "hello world" has 11 chars
    }

    #[test]
    fn graph_editor_dialog_char_len() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "abc".into(),
            crate::tui::app::types::GraphEditorMode::NodeConfig,
        );
        assert_eq!(dialog.char_len(), 3);
        dialog.insert_str("de");
        assert_eq!(dialog.char_len(), 5);
    }

    #[test]
    fn graph_editor_dialog_insert_char() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "ac".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        dialog.cursor = 1;
        dialog.insert_char('b');
        assert_eq!(dialog.buffer, "abc");
        assert_eq!(dialog.cursor, 2);
    }

    #[test]
    fn graph_editor_dialog_backspace() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "abc".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        dialog.backspace();
        assert_eq!(dialog.buffer, "ab");
        assert_eq!(dialog.cursor, 2);
    }

    #[test]
    fn graph_editor_dialog_backspace_at_zero() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "abc".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        dialog.cursor = 0;
        dialog.backspace();
        assert_eq!(dialog.buffer, "abc");
    }

    #[test]
    fn graph_editor_dialog_move_left_right() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "abc".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        dialog.move_left();
        assert_eq!(dialog.cursor, 2);
        dialog.move_right();
        assert_eq!(dialog.cursor, 3);
        dialog.move_right(); // At end
        assert_eq!(dialog.cursor, 3);
    }

    #[test]
    fn graph_editor_dialog_move_home_end() {
        let mut dialog = crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "".into(),
            "".into(),
            "hello".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        );
        dialog.move_home();
        assert_eq!(dialog.cursor, 0);
        dialog.move_end();
        assert_eq!(dialog.cursor, 5);
    }

    // ── Knowledge filter tests ──────────────────────────────────

    #[test]
    fn filtered_knowledge_indices_empty_filter_returns_all() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n1".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Fact One".into(),
                body: "body one".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n2".into(),
                kind: "pattern".into(),
                status: "noted".into(),
                title: "Pattern Two".into(),
                body: "body two".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
        ];
        app.knowledge_filter.clear();
        let indices = app.filtered_knowledge_indices();
        assert_eq!(indices, vec![0, 1]);
    }

    #[test]
    fn filtered_knowledge_indices_filter_matches_title() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n1".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Rust Ownership".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n2".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Python GIL".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
        ];
        app.knowledge_filter = "rust".to_string();
        let indices = app.filtered_knowledge_indices();
        assert_eq!(indices, vec![0]);
    }

    #[test]
    fn filtered_knowledge_indices_filter_matches_body() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![crate::db::intelligence::IntelligenceNodeRecord {
            id: "n1".into(),
            kind: "fact".into(),
            status: "noted".into(),
            title: "Title".into(),
            body: "contains the word pattern".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        }];
        app.knowledge_filter = "pattern".to_string();
        let indices = app.filtered_knowledge_indices();
        assert_eq!(indices, vec![0]);
    }

    #[test]
    fn filtered_knowledge_indices_filter_matches_kind() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n1".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Title".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n2".into(),
                kind: "pattern".into(),
                status: "noted".into(),
                title: "Title".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
        ];
        app.knowledge_filter = "pattern".to_string();
        let indices = app.filtered_knowledge_indices();
        assert_eq!(indices, vec![1]);
    }

    #[test]
    fn filtered_knowledge_indices_no_match() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![crate::db::intelligence::IntelligenceNodeRecord {
            id: "n1".into(),
            kind: "fact".into(),
            status: "noted".into(),
            title: "Title".into(),
            body: "body".into(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: chrono::Utc::now().timestamp(),
            updated_at: chrono::Utc::now().timestamp(),
        }];
        app.knowledge_filter = "zzz_not_found".to_string();
        let indices = app.filtered_knowledge_indices();
        assert!(indices.is_empty());
    }

    // ── ProjectTab tests ────────────────────────────────────────

    #[test]
    fn project_tab_labels() {
        assert_eq!(ProjectTab::Overview.label(), "Overview");
        assert_eq!(ProjectTab::Backlog.label(), "Backlog");
        assert_eq!(ProjectTab::Knowledge.label(), "Knowledge");
        assert_eq!(ProjectTab::History.label(), "History");
    }

    #[test]
    fn project_tab_hotkeys() {
        assert_eq!(ProjectTab::Overview.hotkey(), 'o');
        assert_eq!(ProjectTab::Backlog.hotkey(), 'b');
        assert_eq!(ProjectTab::Knowledge.hotkey(), 'k');
        assert_eq!(ProjectTab::History.hotkey(), 'h');
    }

    #[test]
    fn project_tab_all_has_four_entries() {
        assert_eq!(ProjectTab::ALL.len(), 4);
    }

    // ── AgentEntry::id tests ────────────────────────────────────

    #[test]
    fn agent_entry_id_for_agent() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Agent(crate::domain::models::Agent {
            id: "bg-1".to_string(),
            prompt: String::new(),
            trigger: None,
            cli: crate::domain::models::Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/bg-1.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        });
        assert_eq!(entry.id(&app), "bg-1");
    }

    #[test]
    fn agent_entry_id_for_corrupt() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Corrupt(crate::domain::models::CorruptAgent {
            id: "corrupt-1".to_string(),
            enabled: false,
            error: "corrupt row".to_string(),
        });
        assert_eq!(entry.id(&app), "corrupt-1");
    }

    #[test]
    fn agent_entry_id_for_group_out_of_bounds() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Group(99);
        assert_eq!(entry.id(&app), "?");
    }

    #[test]
    fn agent_entry_id_for_interactive_out_of_bounds() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Interactive(99);
        assert_eq!(entry.id(&app), "?");
    }

    #[test]
    fn agent_entry_id_for_terminal_out_of_bounds() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Terminal(99);
        assert_eq!(entry.id(&app), "?");
    }

    #[test]
    fn agent_entry_id_for_orphaned_out_of_bounds() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let entry = AgentEntry::Orphaned(99);
        assert_eq!(entry.id(&app), "?");
    }

    // ── Navigation edge cases ───────────────────────────────────

    #[test]
    fn select_next_empty_agents_stays_put() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents.clear();
        app.selected = 0;
        app.sidebar_layer = SidebarLayer::Live;
        app.select_next();
        // Should not panic, stays at 0
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_prev_empty_agents_stays_put() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents.clear();
        app.selected = 0;
        app.sidebar_layer = SidebarLayer::Live;
        app.select_prev();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn select_agent_at_out_of_bounds_does_nothing() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents.clear();
        let prev = app.selected;
        app.select_agent_at(999);
        assert_eq!(app.selected, prev);
    }

    #[test]
    fn scroll_log_down_and_up() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.log_scroll = 10;
        app.scroll_log_down();
        assert_eq!(app.log_scroll, 13);
        app.scroll_log_up();
        assert_eq!(app.log_scroll, 10);
    }

    #[test]
    fn scroll_log_up_at_zero_stays_zero() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.log_scroll = 0;
        app.scroll_log_up();
        assert_eq!(app.log_scroll, 0);
    }

    // ── sidebar_graphs: no status filtering ──────────────────────

    #[test]
    fn sidebar_graphs_includes_every_status() {
        use crate::domain::graphs::GraphStatus;
        let db = test_db();
        db.insert_graph(&make_graph("l1", "Running", GraphStatus::Running))
            .unwrap();
        db.insert_graph(&make_graph("l2", "Draft", GraphStatus::Draft))
            .unwrap();
        db.insert_graph(&make_graph("l3", "Paused", GraphStatus::Paused))
            .unwrap();
        db.insert_graph(&make_graph("l4", "Completed", GraphStatus::Completed))
            .unwrap();
        db.insert_graph(&make_graph("l5", "Failed", GraphStatus::Failed))
            .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let listed: Vec<&str> = app
            .sidebar_graphs()
            .iter()
            .map(|lp| lp.id.as_str())
            .collect();
        assert!(
            listed.contains(&"l4"),
            "a completed graph must stay in the sidebar list"
        );
        assert!(
            listed.contains(&"l5"),
            "a failed graph must stay in the sidebar list"
        );
        assert!(listed.contains(&"l1"));
        assert!(listed.contains(&"l2"));
        assert!(listed.contains(&"l3"));
        assert_eq!(listed.len(), 5, "no graph is dropped by status");
    }

    // ── Playground state tests ───────────────────────────────────

    #[test]
    fn activate_deactivate_playground() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(!app.playground_active);

        app.playground_query = "test query".to_string();
        app.playground_selected = 5;
        app.playground_active = true;
        app.activate_playground();
        assert!(app.playground_active);
        assert!(app.playground_query.is_empty());
        assert_eq!(app.playground_selected, 0);

        app.deactivate_playground();
        assert!(!app.playground_active);
        assert!(app.playground_query.is_empty());
    }

    // ── Playground search edge cases ─────────────────────────────

    #[test]
    fn poll_playground_search_no_rx_returns_early() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.playground_search_rx = None;
        // Should not panic
        app.poll_playground_search();
    }

    #[test]
    fn poll_playground_search_disconnected_cleans_up() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let (tx, rx) = std::sync::mpsc::channel();
        app.playground_search_rx = Some(rx);
        app.playground_search_pending = true;
        drop(tx);
        app.poll_playground_search();
        assert!(app.playground_search_rx.is_none());
        assert!(!app.playground_search_pending);
    }

    #[test]
    fn duplicate_selected_graph_node_copies_config_and_opens_editor() {
        use crate::domain::graphs::{
            GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
        };
        use crate::tui::app::types::Focus;

        let db = test_db();
        db.insert_graph(&make_graph("graph-1", "Graph", GraphStatus::Draft))
            .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "spec-a".to_string(),
            graph_id: Some("graph-1".to_string()),
            name: "spec-a".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "impl".to_string(),
            spec_id: Some("spec-a".to_string()),
            graph_id: None,
            name: "implement".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "do it"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.selected_graph_id = Some("graph-1".to_string());
        app.graph_details = db.get_graph_details("graph-1").unwrap();
        app.graph_selected_spec = 0;
        app.graph_selected_node = 0;
        assert_eq!(app.selected_graph_node().unwrap().id, "impl");

        app.duplicate_selected_graph_node().unwrap();

        // The editor opens pre-filled on the new copy.
        assert!(matches!(app.focus, Focus::GraphEditorDialog));
        let dialog = app.graph_editor_dialog.as_ref().unwrap();
        assert!(
            dialog.node_name.ends_with("(copy)"),
            "expected a copy name, got '{}'",
            dialog.node_name
        );
        assert_ne!(dialog.node_id, "impl", "the copy must have a fresh id");

        // A second node now exists on the spec with the source's config.
        let nodes = db.list_graph_nodes("spec-a").unwrap();
        assert_eq!(nodes.len(), 2);
        let copy = nodes.iter().find(|n| n.id != "impl").unwrap();
        assert_eq!(copy.name, "implement (copy)");
        assert_eq!(copy.config["platform"], "claude");
        assert_eq!(copy.config["prompt_template"], "do it");
    }

    // ── Additional navigation and state tests ───────────────────

    #[test]
    fn sidebar_graphs_empty_when_no_graphs() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.sidebar_graphs().is_empty());
    }

    #[test]
    fn live_indices_empty_when_no_agents() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.live_indices().is_empty());
    }

    #[test]
    fn automation_agent_indices_empty_when_no_agents() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.automation_agent_indices().is_empty());
    }

    #[test]
    fn step_sidebar_tab_forward_wraps() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.step_sidebar_tab(true);
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    }

    #[test]
    fn step_sidebar_tab_backward_wraps() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Live;
        app.step_sidebar_tab(false);
        assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    }

    fn bg_agent(id: &str) -> crate::domain::models::Agent {
        crate::domain::models::Agent {
            id: id.to_string(),
            prompt: String::new(),
            trigger: None,
            cli: crate::domain::models::Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: format!("/tmp/{id}.log"),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    #[test]
    fn step_sidebar_tab_restores_non_edge_selection_on_return() {
        // Functional requirement 6: stepping away from a layer and back must
        // not disturb what was selected there, unlike a fresh jump (F2,
        // click), which deliberately always lands on the tab's edge item.
        // `Live` and Automation's agent sub-list share `selected` as their
        // index space, so this specifically exercises the case where
        // leaving one for the other would otherwise clobber it.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![
            AgentEntry::Group(0),
            AgentEntry::Group(1),
            AgentEntry::Agent(bg_agent("bg-1")),
        ];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 1; // the second (non-edge) Live item

        app.step_sidebar_tab(true);
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        assert_eq!(
            app.selected, 2,
            "a fresh jump into Automation lands on its edge item"
        );

        app.step_sidebar_tab(false);
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_eq!(
            app.selected, 1,
            "returning to Live must restore the remembered selection, not reset to its edge"
        );
    }

    #[test]
    fn step_sidebar_tab_falls_back_to_edge_when_remembered_selection_is_gone() {
        // If the remembered index no longer belongs to the layer (e.g. the
        // entry was removed while away), stepping back must not restore a
        // stale/invalid index — it should behave like a fresh jump instead.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![
            AgentEntry::Group(0),
            AgentEntry::Group(1),
            AgentEntry::Agent(bg_agent("bg-1")),
        ];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 1;

        app.step_sidebar_tab(true);
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);

        // The remembered Live index (1) is no longer a Live entry.
        app.agents[1] = AgentEntry::Agent(bg_agent("bg-2"));

        app.step_sidebar_tab(false);
        assert_eq!(app.sidebar_layer, SidebarLayer::Live);
        assert_eq!(
            app.selected, 0,
            "an invalidated memory falls back to the edge item"
        );
    }

    #[test]
    fn reset_log_scroll_sets_to_zero() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.log_scroll = 50;
        app.sidebar_scroll_offset = 10;
        app.reset_log_scroll();
        assert_eq!(app.log_scroll, 0);
        assert_eq!(app.sidebar_scroll_offset, 0);
    }

    #[test]
    fn cycle_sidebar_layer_skips_empty_tabs() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.sidebar_layer = SidebarLayer::Live;
        // No agents, no projects, no graphs → cycle may stay or move
        app.cycle_sidebar_layer();
        // Should not panic
    }

    #[test]
    fn selected_project_out_of_bounds() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.selected_project().is_none());
    }

    #[test]
    fn selected_graph_none_when_no_graphs() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.selected_graph().is_none());
    }

    #[test]
    fn selected_graph_spec_none_when_no_details() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.selected_graph_spec().is_none());
    }

    #[test]
    fn selected_graph_node_none_when_no_details() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.selected_graph_node().is_none());
    }

    #[test]
    fn visible_graphs_returns_all_graphs() {
        use crate::domain::graphs::GraphStatus;
        let db = test_db();
        db.insert_graph(&make_graph("l1", "A", GraphStatus::Running))
            .unwrap();
        db.insert_graph(&make_graph("l2", "B", GraphStatus::Completed))
            .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert_eq!(app.visible_graphs().len(), 2);
    }

    #[test]
    fn selected_agent_none_when_empty() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.selected_agent().is_none());
    }

    #[test]
    fn selected_id_empty_when_no_agent() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert_eq!(app.selected_id(), "—");
    }

    #[test]
    fn focused_agent_name_empty_when_no_agent() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.focused_agent_name().is_empty());
    }

    #[test]
    fn graph_live_highlighted_node_id_none_when_no_live_state() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(app.graph_live_highlighted_node_id().is_none());
    }

    #[test]
    fn graph_live_reset_follow_clears_selection() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("some-node".to_string());
        app.graph_live_reset_follow();
        assert!(app.graph_live_follow);
        assert!(app.graph_live_selected_node.is_none());
    }

    #[test]
    fn graph_live_highlighted_node_run_info_default_when_no_live_state() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let info = app.graph_live_highlighted_node_run_info();
        assert!(info.status.is_none());
    }

    fn spec_queue_entry(id: &str, status: GraphSpecStatus) -> SpecQueueEntry {
        SpecQueueEntry {
            spec_id: id.to_string(),
            spec_name: format!("Spec {id}"),
            status,
            failure_reason: None,
            spec_code: None,
            spec_type: None,
            spec_scope: None,
        }
    }

    fn app_with_spec_queue(ids: &[&str]) -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_state = Some(GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "graph".to_string(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: ids
                .iter()
                .map(|id| spec_queue_entry(id, GraphSpecStatus::Pending))
                .collect(),
            done_count: 0,
            total_count: ids.len(),
            current_spec_id: None,
            effective_nodes: Vec::new(),
            effective_edges: Vec::new(),
            ensembles: Vec::new(),
            router_taken_routes: HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        });
        app
    }

    #[test]
    fn graph_spec_strip_move_selection_cycles_through_queue_and_wraps() {
        let mut app = app_with_spec_queue(&["s1", "s2", "s3"]);
        assert!(app.graph_spec_strip_selected.is_none());

        app.graph_spec_strip_move_selection(true);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s1"));
        app.graph_spec_strip_move_selection(true);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s2"));
        app.graph_spec_strip_move_selection(true);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
        // Wraps back to the first spec.
        app.graph_spec_strip_move_selection(true);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s1"));

        // Backward wraps the other way.
        app.graph_spec_strip_move_selection(false);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));

        // Selecting a spec by hand (the mouse click path) and then moving by
        // keyboard from that point lands on the same spec a second keyboard
        // move would — click and keyboard share one selection.
        app.graph_spec_strip_select("s2".to_string());
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s2"));
        app.graph_spec_strip_move_selection(true);
        assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
    }

    #[test]
    fn graph_spec_strip_move_selection_never_touches_graph_follow() {
        let mut app = app_with_spec_queue(&["s1", "s2"]);
        assert!(app.graph_live_follow);
        app.graph_spec_strip_move_selection(true);
        assert!(
            app.graph_live_follow,
            "selecting a spec in the strip must not disturb the graph's own follow state"
        );
    }

    #[test]
    fn graph_spec_strip_select_ignores_unknown_spec_id() {
        let mut app = app_with_spec_queue(&["s1", "s2"]);
        app.graph_spec_strip_select("does-not-exist".to_string());
        assert!(app.graph_spec_strip_selected.is_none());
    }

    #[test]
    fn graph_spec_strip_select_focuses_the_strip() {
        let mut app = app_with_spec_queue(&["s1", "s2"]);
        assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
        app.graph_spec_strip_select("s1".to_string());
        assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
    }

    #[test]
    fn graph_live_toggle_focus_toggles_between_graph_and_spec_strip() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
        app.graph_live_toggle_focus();
        assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
        app.graph_live_toggle_focus();
        assert_eq!(app.graph_live_focus, GraphLiveFocus::Graph);
    }

    #[test]
    fn cancel_graph_editor_dialog() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_editor_dialog = Some(crate::tui::app::types::GraphEditorDialog::new(
            "n1".into(),
            "node1".into(),
            "title".into(),
            "help".into(),
            "buffer".into(),
            crate::tui::app::types::GraphEditorMode::AgentPrompt,
        ));
        app.cancel_graph_editor_dialog();
        assert!(app.graph_editor_dialog.is_none());
        assert!(matches!(app.focus, Focus::Preview));
    }

    #[test]
    fn toggle_rag_pause_flips() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(!app.rag_paused);
        app.toggle_rag_pause();
        assert!(app.rag_paused);
        app.toggle_rag_pause();
        assert!(!app.rag_paused);
    }

    #[test]
    fn normalize_selected_knowledge_clamps_to_first_filtered() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.project_knowledge = vec![
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n1".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Alpha".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
            crate::db::intelligence::IntelligenceNodeRecord {
                id: "n2".into(),
                kind: "fact".into(),
                status: "noted".into(),
                title: "Beta".into(),
                body: "body".into(),
                metadata: None,
                project_hash: None,
                session_id: None,
                created_at: chrono::Utc::now().timestamp(),
                updated_at: chrono::Utc::now().timestamp(),
            },
        ];
        app.selected_knowledge = 5; // out of range
        app.knowledge_filter = "alpha".to_string();
        app.normalize_selected_knowledge();
        assert_eq!(app.selected_knowledge, 0);
    }

    #[test]
    fn enter_exit_knowledge_filter_mode() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        assert!(!app.knowledge_filter_mode);
        app.enter_knowledge_filter_mode();
        assert!(app.knowledge_filter_mode);
        app.exit_knowledge_filter_mode();
        assert!(!app.knowledge_filter_mode);
    }

    #[test]
    fn dismiss_copied_recent_does_not_clear() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.show_copied = true;
        app.copied_at = std::time::Instant::now();
        app.dismiss_copied();
        assert!(app.show_copied);
    }

    #[test]
    fn dismiss_copied_old_enough_clears() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.show_copied = true;
        app.copied_at = std::time::Instant::now() - std::time::Duration::from_secs(5);
        app.dismiss_copied();
        assert!(!app.show_copied);
    }

    // ── Edges dialog (retarget/delete an ordinary edge) ───────────────

    /// Seeds a `Draft` graph with three plain agent nodes `A -> B -> C`
    /// (`pass` edges) — the fixture the `Edges` dialog tests in this
    /// section start from. `A` has no incoming edge (the graph's entry
    /// point); only `A -> B` is wired, so there's a spare target (`C`) to
    /// retarget onto.
    fn seed_plain_edge_graph(db: &Database) {
        use crate::domain::graphs::{
            Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpec,
        };

        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "plp1".to_string(),
            name: "plain graph".to_string(),
            description: None,
            workdir: "/tmp/plain-edge-test".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "ps1".to_string(),
            graph_id: Some("plp1".to_string()),
            name: "spec one".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        for (id, name, position) in [("node_a", "A", 0), ("node_b", "B", 1), ("node_c", "C", 2)] {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: Some("ps1".to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position,
                created_at: chrono::Utc::now(),
            })
            .unwrap();
        }
        db.insert_graph_edge(&GraphEdge {
            id: "e_ab".to_string(),
            spec_id: Some("ps1".to_string()),
            graph_id: None,
            from_node: "node_a".to_string(),
            to_node: "node_b".to_string(),
            condition: GraphEdgeCondition::Pass,
        })
        .unwrap();
    }

    fn app_on_plain_node(db: &Arc<Database>, data_dir: &std::path::Path, node_index: usize) -> App {
        let mut app = App::new(
            Arc::clone(db),
            data_dir,
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_graphs().expect("refresh graphs");
        app.graph_selected_spec = 0;
        app.graph_selected_node = node_index;
        app
    }

    #[test]
    fn open_graph_edges_dialog_lists_the_nodes_outgoing_plain_edges() {
        let db = test_db();
        seed_plain_edge_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_plain_node(&db, data_dir.path(), 0);

        app.open_graph_edges_dialog().expect("open edges dialog");

        let dialog = app.graph_editor_dialog.as_ref().expect("dialog opens");
        assert!(matches!(
            dialog.mode,
            crate::tui::app::types::GraphEditorMode::Edges
        ));
        assert_eq!(dialog.edge_rows.len(), 1);
        assert_eq!(dialog.edge_rows[0].to_node, "node_b");
        assert!(
            !dialog.edge_targets.iter().any(|(id, _)| id == "node_a"),
            "candidate targets exclude the node itself"
        );
    }

    #[test]
    fn retarget_focused_graph_edge_changes_only_the_destination() {
        let db = test_db();
        seed_plain_edge_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_plain_node(&db, data_dir.path(), 0);
        app.open_graph_edges_dialog().expect("open edges dialog");

        app.retarget_focused_graph_edge(true).expect("retarget");

        let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
        assert!(dialog.parse_error.is_none(), "{:?}", dialog.parse_error);
        assert_eq!(dialog.edge_rows[0].to_node, "node_c");
        assert_eq!(
            dialog.edge_rows[0].condition,
            crate::domain::graphs::GraphEdgeCondition::Pass,
            "retargeting never touches the edge's condition"
        );

        let edge = db.get_graph_edge("e_ab").unwrap().unwrap();
        assert_eq!(edge.to_node, "node_c");
        assert_eq!(edge.from_node, "node_a");
    }

    #[test]
    fn delete_focused_graph_edge_removes_it_from_the_db_and_the_dialog() {
        let db = test_db();
        seed_plain_edge_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_plain_node(&db, data_dir.path(), 0);
        app.open_graph_edges_dialog().expect("open edges dialog");

        app.delete_focused_graph_edge().expect("delete edge");

        let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
        assert!(dialog.edge_rows.is_empty());
        assert!(db.get_graph_edge("e_ab").unwrap().is_none());
    }

    #[test]
    fn retarget_focused_graph_edge_surfaces_the_running_graph_rejection() {
        let db = test_db();
        seed_plain_edge_graph(&db);
        db.update_graph_status(
            "plp1",
            crate::domain::graphs::GraphStatus::Running,
            None,
            None,
        )
        .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_plain_node(&db, data_dir.path(), 0);
        app.open_graph_edges_dialog().expect("open edges dialog");

        app.retarget_focused_graph_edge(true)
            .expect("call returns Ok");

        let dialog = app.graph_editor_dialog.as_ref().expect("dialog still open");
        let error = dialog.parse_error.as_deref().unwrap_or_default();
        assert!(error.contains("running"), "{error}");
        // Target unchanged in the db.
        assert_eq!(
            db.get_graph_edge("e_ab").unwrap().unwrap().to_node,
            "node_b"
        );
    }

    // ── Router routes dialog (open/pre-fill/save) ────────────────────

    /// Seeds a `Draft` graph with one spec containing a 4-route router node
    /// ("billing"/"technical"/"sales"/"escalation", fallback "escalation")
    /// already wired to `billing`, plus three plain agent target nodes —
    /// the fixture every router-dialog test in this section starts from.
    fn seed_router_graph(db: &Database) {
        use crate::domain::graphs::{
            Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpec,
        };

        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "rlp1".to_string(),
            name: "router graph".to_string(),
            description: None,
            workdir: "/tmp/router-test".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "rs1".to_string(),
            graph_id: Some("rlp1".to_string()),
            name: "spec one".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();

        db.insert_graph_node(&GraphNode {
            id: "router".to_string(),
            spec_id: Some("rs1".to_string()),
            graph_id: None,
            name: "Classify".to_string(),
            kind: GraphNodeKind::Router,
            config: serde_json::json!({
                "routes": [
                    {"label": "billing", "description": "billing desc"},
                    {"label": "technical", "description": "technical desc"},
                    {"label": "sales", "description": "sales desc"},
                    {"label": "escalation", "description": "escalation desc"},
                ],
                "fallback": "escalation",
            }),
            position: 0,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        for (id, name, position) in [
            ("billing_agent", "Billing specialist", 1),
            ("technical_agent", "Technical specialist", 2),
            ("sales_agent", "Sales specialist", 3),
        ] {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: Some("rs1".to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position,
                created_at: chrono::Utc::now(),
            })
            .unwrap();
        }
        db.insert_graph_edge(&GraphEdge {
            id: "e_billing".to_string(),
            spec_id: Some("rs1".to_string()),
            graph_id: None,
            from_node: "router".to_string(),
            to_node: "billing_agent".to_string(),
            condition: GraphEdgeCondition::Route("billing".to_string()),
        })
        .unwrap();
    }

    fn app_on_router_node(db: &Arc<Database>, data_dir: &std::path::Path) -> App {
        let mut app = App::new(
            Arc::clone(db),
            data_dir,
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_graphs().expect("refresh graphs");
        app.graph_selected_spec = 0;
        app.graph_selected_node = 0; // "router" is position 0
        assert_eq!(
            app.selected_graph_node().map(|n| n.id.as_str()),
            Some("router"),
            "fixture invariant: router node must be selected"
        );
        app
    }

    #[test]
    fn open_graph_editor_dialog_prefills_router_routes_fallback_and_existing_wiring() {
        let db = test_db();
        seed_router_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_router_node(&db, data_dir.path());

        app.open_graph_editor_dialog().expect("open editor");

        let dialog = app.graph_editor_dialog.as_ref().expect("dialog opens");
        assert!(matches!(
            dialog.mode,
            crate::tui::app::types::GraphEditorMode::RouterRoutes
        ));
        assert_eq!(dialog.router_routes.len(), 4);
        assert_eq!(dialog.router_fallback, "escalation");

        let billing = dialog
            .router_routes
            .iter()
            .find(|r| r.label == "billing")
            .expect("billing route present");
        assert_eq!(billing.description, "billing desc");
        assert_eq!(billing.target_node_id.as_deref(), Some("billing_agent"));

        let technical = dialog
            .router_routes
            .iter()
            .find(|r| r.label == "technical")
            .expect("technical route present");
        assert_eq!(
            technical.target_node_id, None,
            "not yet wired in the fixture"
        );

        // Candidate targets exclude the router itself.
        assert!(!dialog.router_targets.iter().any(|(id, _)| id == "router"));
        assert!(dialog
            .router_targets
            .iter()
            .any(|(id, _)| id == "technical_agent"));
    }

    #[test]
    fn save_router_routes_dialog_rejects_an_invalid_fallback_with_a_readable_message() {
        let db = test_db();
        seed_router_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_router_node(&db, data_dir.path());
        app.open_graph_editor_dialog().expect("open editor");

        app.graph_editor_dialog.as_mut().unwrap().router_fallback = "not-a-route".to_string();

        let result = app.save_graph_editor_dialog();
        assert!(result.is_err(), "invalid fallback must not save");

        let dialog = app
            .graph_editor_dialog
            .as_ref()
            .expect("dialog stays open on validation failure");
        let message = dialog.parse_error.as_deref().unwrap_or_default();
        assert!(
            message.contains("not-a-route"),
            "error should be a readable, specific message: {message:?}"
        );

        // Nothing was written.
        let node = db.get_graph_node("router").unwrap().unwrap();
        assert_eq!(node.config["fallback"], "escalation");
    }

    #[test]
    fn save_router_routes_dialog_rejects_partial_wiring_before_writing_anything() {
        let db = test_db();
        seed_router_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_router_node(&db, data_dir.path());
        app.open_graph_editor_dialog().expect("open editor");

        // "billing" is already wired (from the fixture); leaving every other
        // route unwired is a rejected half-wired state, not a valid save.
        let result = app.save_graph_editor_dialog();
        assert!(result.is_err());
        assert!(app
            .graph_editor_dialog
            .as_ref()
            .unwrap()
            .parse_error
            .is_some());

        // The pre-existing edge is untouched.
        let edges = db.list_graph_edges("rs1").unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to_node, "billing_agent");
    }

    #[test]
    fn save_router_routes_dialog_persists_config_and_wires_every_route() {
        let db = test_db();
        seed_router_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_router_node(&db, data_dir.path());
        app.open_graph_editor_dialog().expect("open editor");

        {
            let dialog = app.graph_editor_dialog.as_mut().unwrap();
            for route in dialog.router_routes.iter_mut() {
                route.target_node_id = Some(match route.label.as_str() {
                    "billing" => "billing_agent".to_string(),
                    "technical" => "technical_agent".to_string(),
                    "sales" => "sales_agent".to_string(),
                    "escalation" => "billing_agent".to_string(),
                    other => panic!("unexpected route {other}"),
                });
            }
        }

        app.save_graph_editor_dialog().expect("save succeeds");

        assert!(app.graph_editor_dialog.is_none(), "dialog closes on save");

        let node = db.get_graph_node("router").unwrap().unwrap();
        assert_eq!(node.config["fallback"], "escalation");
        assert_eq!(node.config["routes"].as_array().unwrap().len(), 4);

        let edges = db.list_graph_edges("rs1").unwrap();
        assert_eq!(edges.len(), 4, "every route now has exactly one edge");
        let by_route: HashMap<&str, &str> = edges
            .iter()
            .map(|e| {
                (
                    e.condition.route_label().expect("route edge"),
                    e.to_node.as_str(),
                )
            })
            .collect();
        assert_eq!(by_route["billing"], "billing_agent");
        assert_eq!(by_route["technical"], "technical_agent");
        assert_eq!(by_route["sales"], "sales_agent");
        assert_eq!(by_route["escalation"], "billing_agent");
    }

    #[test]
    fn save_router_routes_dialog_drops_the_edge_of_a_removed_route() {
        let db = test_db();
        seed_router_graph(&db);
        let data_dir = tempdir().expect("create data dir");
        let mut app = app_on_router_node(&db, data_dir.path());
        app.open_graph_editor_dialog().expect("open editor");

        {
            let dialog = app.graph_editor_dialog.as_mut().unwrap();
            // Drop "billing" — the one route the fixture already wired.
            let idx = dialog
                .router_routes
                .iter()
                .position(|r| r.label == "billing")
                .unwrap();
            dialog.router_route_index = idx;
            dialog.router_remove_route();
            // The remaining 3 routes must all be wired for this to be a
            // valid, non-partial save.
            for route in dialog.router_routes.iter_mut() {
                route.target_node_id = Some("technical_agent".to_string());
            }
            dialog.router_fallback = "escalation".to_string();
        }

        app.save_graph_editor_dialog().expect("save succeeds");

        let edges = db.list_graph_edges("rs1").unwrap();
        assert_eq!(edges.len(), 3);
        assert!(
            !edges
                .iter()
                .any(|e| e.condition.route_label() == Some("billing")),
            "the removed route's edge must not survive the save"
        );
    }

    /// `NodeConfig` mode is the one TUI path that hand-writes a node's raw
    /// config JSON (a `check` node here — agent nodes go through the
    /// structured `AgentPrompt` mode instead), so it's the one that can
    /// reintroduce the exact incident shape (a config key the engine will
    /// never read) if left unvalidated. Saving must be rejected the same
    /// way `graph_add_node`/`graph_update_node` reject it, and the bad config
    /// must never reach the DB.
    #[test]
    fn save_graph_editor_dialog_rejects_unknown_config_key_in_node_config_mode() {
        use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphSpec};

        let db = test_db();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "clp1".to_string(),
            name: "check graph".to_string(),
            description: None,
            workdir: "/tmp/check-test".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "cs1".to_string(),
            graph_id: Some("clp1".to_string()),
            name: "spec one".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "check1".to_string(),
            spec_id: Some("cs1".to_string()),
            graph_id: None,
            name: "Gate".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 0,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_graphs().expect("refresh graphs");
        app.graph_selected_spec = 0;
        app.graph_selected_node = 0;
        assert_eq!(
            app.selected_graph_node().map(|n| n.id.as_str()),
            Some("check1"),
            "fixture invariant: check node must be selected"
        );

        app.open_graph_editor_dialog().expect("open editor");
        assert!(matches!(
            app.graph_editor_dialog.as_ref().unwrap().mode,
            crate::tui::app::types::GraphEditorMode::NodeConfig
        ));
        app.graph_editor_dialog.as_mut().unwrap().buffer =
            serde_json::json!({"command": "true", "unexpected_field": true}).to_string();

        let result = app.save_graph_editor_dialog();
        assert!(result.is_err(), "an unrecognized config key must not save");

        let dialog = app
            .graph_editor_dialog
            .as_ref()
            .expect("dialog reopens with the error visible");
        assert!(
            dialog
                .parse_error
                .as_deref()
                .unwrap_or_default()
                .contains("unexpected_field"),
            "{:?}",
            dialog.parse_error
        );

        let stored = db.get_graph_node("check1").unwrap().unwrap();
        assert_eq!(
            stored.config,
            serde_json::json!({"command": "true"}),
            "the invalid config must never reach the DB"
        );
    }

    #[test]
    fn graph_live_view_scroll_step_clamps_at_zero() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_view_scroll = 0;
        app.graph_live_view_total_lines = 50;
        app.last_panel_inner = (80, 20);
        app.graph_live_view_scroll_step(-10);
        assert_eq!(app.graph_live_view_scroll, 0, "scroll must not go below 0");
    }

    #[test]
    fn graph_live_view_scroll_step_clamps_at_max() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_view_total_lines = 50;
        app.last_panel_inner = (80, 20);
        app.graph_live_view_scroll = 30;
        app.graph_live_view_scroll_step(10);
        assert_eq!(
            app.graph_live_view_scroll, 30,
            "scroll must not exceed total_lines - height (30)"
        );
        app.graph_live_view_scroll = 10;
        app.graph_live_view_scroll_step(5);
        assert_eq!(app.graph_live_view_scroll, 15);
        app.graph_live_view_scroll_step(100);
        assert_eq!(app.graph_live_view_scroll, 30);
    }
    // --- CT2 navigation tests ---

    fn graph_state(
        nodes: &[(&str, &str)],
        edges: &[(&str, &str, crate::domain::graphs::GraphEdgeCondition)],
        current: Option<&str>,
    ) -> GraphLiveState {
        use crate::domain::graphs::{GraphEdge, GraphNode, GraphNodeKind};
        use chrono::Utc;
        use serde_json::json;
        let effective_nodes = nodes
            .iter()
            .enumerate()
            .map(|(i, (id, name))| GraphNode {
                id: id.to_string(),
                spec_id: Some("s1".to_string()),
                graph_id: None,
                name: name.to_string(),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .collect::<Vec<_>>();
        let effective_edges = edges
            .iter()
            .enumerate()
            .map(|(i, (from, to, cond))| GraphEdge {
                id: format!("e{i}"),
                spec_id: Some("s1".to_string()),
                graph_id: None,
                from_node: from.to_string(),
                to_node: to.to_string(),
                condition: (*cond).clone(),
            })
            .collect::<Vec<_>>();
        GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "graph".to_string(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: Some("s1".to_string()),
            effective_nodes,
            effective_edges,
            ensembles: Vec::new(),
            router_taken_routes: HashMap::new(),
            current_node_id: current.map(|s| s.to_string()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        }
    }

    #[test]
    fn navigate_child_follows_pass_edge() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B"), ("c", "C")],
            &[
                ("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass),
                ("a", "c", crate::domain::graphs::GraphEdgeCondition::Fail),
            ],
            Some("a"),
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = true; // starts auto-following at a
                                      // child should follow pass edge to b
        app.graph_live_navigate_child();
        assert_eq!(app.graph_live_highlighted_node_id(), Some("b"));
        assert!(!app.graph_live_follow);
    }

    #[test]
    fn navigate_parent_follows_incoming_edge() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B")],
            &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
            Some("b"),
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("b".to_string());
        app.graph_live_navigate_parent();
        assert_eq!(app.graph_live_highlighted_node_id(), Some("a"));
    }

    #[test]
    fn navigate_child_no_op_at_leaf() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(&[("a", "A")], &[], Some("a"));
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("a".to_string());
        app.graph_live_navigate_child();
        assert_eq!(
            app.graph_live_highlighted_node_id(),
            Some("a"),
            "leaf child should be no-op"
        );
    }

    #[test]
    fn navigate_parent_no_op_at_root() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B")],
            &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
            Some("a"),
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("a".to_string());
        app.graph_live_navigate_parent();
        assert_eq!(
            app.graph_live_highlighted_node_id(),
            Some("a"),
            "root parent should be no-op"
        );
    }

    #[test]
    fn navigate_child_with_no_highlight_selects_dfs_first() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B")],
            &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
            None,
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;

        app.graph_live_navigate_child();

        assert_eq!(
            app.graph_live_highlighted_node_id(),
            Some("a"),
            "no highlight: child enters at the DFS entry node"
        );
        assert!(!app.graph_live_follow);
    }

    #[test]
    fn navigate_child_through_ensemble_member_target() {
        use crate::domain::graphs::{GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind};
        use crate::tui::app::graph_live_state::{EnsembleLiveInfo, EnsembleMemberLiveInfo};
        use chrono::Utc;
        use serde_json::json;

        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let node = |id: &str, pos: i64| GraphNode {
            id: id.to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            name: id.to_string(),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: pos,
            created_at: Utc::now(),
        };
        let edge = |id: &str, from: &str, to: &str| GraphEdge {
            id: id.to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition: GraphEdgeCondition::Pass,
        };
        let state = GraphLiveState {
            graph_id: "lp1".to_string(),
            graph_name: "graph".to_string(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: Some("s1".to_string()),
            effective_nodes: vec![
                node("start", 0),
                node("m1", 1),
                node("m2", 2),
                node("join1", 3),
            ],
            effective_edges: vec![
                edge("e1", "start", "m1"),
                edge("e2", "m1", "join1"),
                edge("e3", "m2", "join1"),
            ],
            ensembles: vec![EnsembleLiveInfo {
                ensemble_id: "ens1".to_string(),
                name: "E".to_string(),
                join_node_id: "join1".to_string(),
                members: vec![
                    EnsembleMemberLiveInfo {
                        node_id: "m1".to_string(),
                        label: "claude".to_string(),
                        status: None,
                    },
                    EnsembleMemberLiveInfo {
                        node_id: "m2".to_string(),
                        label: "opencode".to_string(),
                        status: None,
                    },
                ],
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }],
            router_taken_routes: HashMap::new(),
            current_node_id: Some("start".to_string()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        };
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("start".to_string());

        // start -> m1 (a member) collapses to ens1, whose first member is m1.
        app.graph_live_navigate_child();
        assert_eq!(app.graph_live_highlighted_node_id(), Some("m1"));

        // From the ensemble member, the parent is the external start node.
        app.graph_live_navigate_parent();
        assert_eq!(app.graph_live_highlighted_node_id(), Some("start"));
    }

    #[test]
    fn navigate_sibling_follows_dfs_order() {
        // Graph: a pass->b, a fail->c, b pass->d
        // DFS order should be a,b,d,c
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")],
            &[
                ("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass),
                ("a", "c", crate::domain::graphs::GraphEdgeCondition::Fail),
                ("b", "d", crate::domain::graphs::GraphEdgeCondition::Pass),
            ],
            Some("a"),
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("a".to_string());
        let expected = vec!["b", "d", "c"];
        for exp in expected {
            app.graph_live_navigate_sibling(true);
            assert_eq!(
                app.graph_live_highlighted_node_id(),
                Some(exp),
                "sibling order mismatch"
            );
        }
        // wrap? sibling move_index wraps, so next should go to a again
        app.graph_live_navigate_sibling(true);
        assert_eq!(app.graph_live_highlighted_node_id(), Some("a"));
    }

    #[test]
    fn graph_live_enter_seeds_from_most_recent_run_on_finished_graph() {
        // CT24 FR1 (b): on a finished graph with recorded runs, Enter seeds
        // the manual selection at the most recent run's node of the
        // last-queued (or strip-selected) spec — not the entry node.
        use crate::domain::graphs::{
            Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun,
            GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
        };
        let db = test_db();
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp1".to_string(),
            name: "Nightly review".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Completed,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "s1".to_string(),
            graph_id: Some("lp1".to_string()),
            name: "Spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Completed,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        for (position, id) in ["n1", "n2"].iter().enumerate() {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: id.to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: position as i64,
                created_at: chrono::Utc::now(),
            })
            .unwrap();
        }
        db.insert_graph_edge(&GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: GraphEdgeCondition::Pass,
        })
        .unwrap();
        // One completed run on n2 for s1: the "most recent run" the seed
        // must prefer over the entry node.
        db.insert_graph_run(&GraphNodeRun {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: "lp1".to_string(),
            spec_id: "s1".to_string(),
            node_id: "n2".to_string(),
            status: GraphRunStatus::Pass,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 0,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let details = db
            .get_graph_details("lp1")
            .ok()
            .flatten()
            .expect("seeded graph resolves");
        app.graph_live_state =
            crate::tui::app::graph_live_state::assemble_graph_live_state(&db, &details);
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;
        app.graph_spec_strip_selected = None;

        app.graph_live_enter();
        assert!(!app.graph_live_follow, "Enter must switch to manual mode");
        assert_eq!(
            app.graph_live_selected_node.as_deref(),
            Some("n2"),
            "seed must be the most recent run's node, not the entry node"
        );
    }

    #[test]
    fn graph_live_enter_is_noop_when_not_following() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_follow = false;
        app.graph_live_selected_node = Some("kept".to_string());

        app.graph_live_enter();

        assert!(!app.graph_live_follow, "already manual: no change");
        assert_eq!(app.graph_live_selected_node.as_deref(), Some("kept"));
    }

    #[test]
    fn graph_live_enter_prefers_current_node_on_running_graph() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B")],
            &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
            Some("b"),
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;

        app.graph_live_enter();

        assert!(!app.graph_live_follow);
        assert_eq!(
            app.graph_live_selected_node.as_deref(),
            Some("b"),
            "running graph seeds the engine's current node"
        );
    }

    #[test]
    fn graph_live_enter_falls_back_to_dfs_entry_on_finished_graph_without_runs() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let state = graph_state(
            &[("a", "A"), ("b", "B")],
            &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
            None,
        );
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;
        app.graph_spec_strip_selected = None;

        app.graph_live_enter();

        assert_eq!(
            app.graph_live_selected_node.as_deref(),
            Some("a"),
            "no runs: fall back to the DFS entry node"
        );
        assert!(!app.graph_live_follow);
    }

    #[test]
    fn graph_live_enter_keeps_follow_when_graph_has_no_nodes() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.graph_live_state = Some(graph_state(&[], &[], None));
        app.graph_live_follow = true;
        app.graph_live_selected_node = None;

        app.graph_live_enter();

        assert!(app.graph_live_follow, "no nodes: stay in auto-follow");
        assert!(app.graph_live_selected_node.is_none());
    }

    // ── dfs tests ───────────────────────────────────────────────────────

    #[test]
    fn dfs_endpoint_resolves_join_node_to_ensemble_id() {
        use crate::tui::app::graph_live_state::EnsembleLiveInfo;
        use std::collections::HashMap;

        let ens = EnsembleLiveInfo {
            ensemble_id: "ens1".into(),
            name: "Ensemble 1".into(),
            join_node_id: "join1".into(),
            members: vec![],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        };
        let ensemble_by_join: HashMap<&str, &EnsembleLiveInfo> = [("join1", &ens)].into();
        let member_to_ensemble: HashMap<String, String> = HashMap::new();
        let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

        let result = App::dfs_endpoint("join1", &ensemble_by_join, &member_to_ensemble, &collapsed);
        assert_eq!(result, Some("ens1".into()));
    }

    #[test]
    fn dfs_endpoint_resolves_ensemble_member_to_ensemble_id() {
        use crate::tui::app::graph_live_state::EnsembleLiveInfo;
        use std::collections::HashMap;

        let ens = EnsembleLiveInfo {
            ensemble_id: "ens1".into(),
            name: "Ensemble 1".into(),
            join_node_id: "join1".into(),
            members: vec![crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                node_id: "m1".into(),
                label: "claude".into(),
                status: None,
            }],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        };
        let ensemble_by_join: HashMap<&str, &EnsembleLiveInfo> = [("join1", &ens)].into();
        let mut member_to_ensemble: HashMap<String, String> = HashMap::new();
        member_to_ensemble.insert("m1".into(), "ens1".into());
        let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

        let result = App::dfs_endpoint("m1", &ensemble_by_join, &member_to_ensemble, &collapsed);
        assert_eq!(result, Some("ens1".into()));
    }

    #[test]
    fn dfs_endpoint_resolves_regular_node_in_collapsed() {
        use std::collections::HashMap;

        let ensemble_by_join: HashMap<&str, &crate::tui::app::graph_live_state::EnsembleLiveInfo> =
            HashMap::new();
        let member_to_ensemble: HashMap<String, String> = HashMap::new();
        let mut collapsed: HashMap<String, (bool, i64)> = HashMap::new();
        collapsed.insert("regular".into(), (false, 0));

        let result = App::dfs_endpoint(
            "regular",
            &ensemble_by_join,
            &member_to_ensemble,
            &collapsed,
        );
        assert_eq!(result, Some("regular".into()));
    }

    #[test]
    fn dfs_endpoint_returns_none_for_unknown_node() {
        use std::collections::HashMap;

        let ensemble_by_join: HashMap<&str, &crate::tui::app::graph_live_state::EnsembleLiveInfo> =
            HashMap::new();
        let member_to_ensemble: HashMap<String, String> = HashMap::new();
        let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

        let result = App::dfs_endpoint(
            "unknown",
            &ensemble_by_join,
            &member_to_ensemble,
            &collapsed,
        );
        assert_eq!(result, None);
    }

    /// dfs_order collapses ensemble members to the first member's node id
    #[test]
    fn dfs_order_collapses_ensemble_members_to_first_member() {
        use crate::domain::graphs::{
            GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpecStatus,
        };
        use crate::tui::app::graph_live_state::{
            EnsembleLiveInfo, EnsembleMemberLiveInfo, GraphLiveState, SpecQueueEntry,
        };
        use chrono::Utc;
        use std::collections::HashMap;

        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let state = GraphLiveState {
            graph_id: "g1".into(),
            graph_name: "Test".into(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".into(),
            trigger_type: "manual".into(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![SpecQueueEntry {
                spec_id: "s1".into(),
                spec_name: "S1".into(),
                status: GraphSpecStatus::Running,
                failure_reason: None,
                spec_code: None,
                spec_type: None,
                spec_scope: None,
            }],
            done_count: 0,
            total_count: 1,
            current_spec_id: Some("s1".into()),
            effective_nodes: vec![
                GraphNode {
                    id: "m1".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Member 1".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                GraphNode {
                    id: "m2".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Member 2".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
                GraphNode {
                    id: "join1".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Join".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 2,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![
                GraphEdge {
                    id: "e1".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    from_node: "m1".into(),
                    to_node: "m2".into(),
                    condition: GraphEdgeCondition::Pass,
                },
                GraphEdge {
                    id: "e2".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    from_node: "m2".into(),
                    to_node: "join1".into(),
                    condition: GraphEdgeCondition::Pass,
                },
            ],
            ensembles: vec![EnsembleLiveInfo {
                ensemble_id: "ens1".into(),
                name: "Ensemble 1".into(),
                join_node_id: "join1".into(),
                members: vec![
                    EnsembleMemberLiveInfo {
                        node_id: "m1".into(),
                        label: "claude".into(),
                        status: None,
                    },
                    EnsembleMemberLiveInfo {
                        node_id: "m2".into(),
                        label: "opencode".into(),
                        status: None,
                    },
                ],
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }],
            router_taken_routes: HashMap::new(),
            current_node_id: Some("m1".into()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        };
        app.graph_live_state = Some(state);

        let order = app.dfs_order();
        // Ensemble members should be collapsed to first member (m1)
        // The final order contains concrete ids, so ens1 -> m1
        assert!(order.contains(&"m1".to_string()));
        // m2 and join1 should not appear in final order
        assert!(!order.contains(&"m2".to_string()));
        assert!(!order.contains(&"join1".to_string()));
        // m1 should appear exactly once
        assert_eq!(order.iter().filter(|n| *n == "m1").count(), 1);
    }

    /// collapse_nodes skips join node ids
    #[test]
    fn collapse_nodes_skips_join_node_ids() {
        use crate::domain::graphs::{GraphNode, GraphNodeKind};
        use crate::tui::app::graph_live_state::{
            EnsembleLiveInfo, EnsembleMemberLiveInfo, GraphLiveState,
        };
        use chrono::Utc;

        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let _app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let state = GraphLiveState {
            graph_id: "g1".into(),
            graph_name: "Test".into(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp".into(),
            trigger_type: "manual".into(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![],
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: vec![
                GraphNode {
                    id: "m1".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Member 1".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 0,
                    created_at: Utc::now(),
                },
                GraphNode {
                    id: "join1".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Join".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 1,
                    created_at: Utc::now(),
                },
                GraphNode {
                    id: "regular".into(),
                    spec_id: Some("s1".into()),
                    graph_id: Some("g1".into()),
                    name: "Regular".into(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({}),
                    position: 2,
                    created_at: Utc::now(),
                },
            ],
            effective_edges: vec![],
            ensembles: vec![EnsembleLiveInfo {
                ensemble_id: "ens1".into(),
                name: "Ensemble 1".into(),
                join_node_id: "join1".into(),
                members: vec![EnsembleMemberLiveInfo {
                    node_id: "m1".into(),
                    label: "claude".into(),
                    status: None,
                }],
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
            }],
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        };

        let collapsed = App::collapse_nodes(&state);
        // join1 should be skipped
        assert!(!collapsed.contains_key("join1"));
        // m1 should be collapsed into ensemble
        assert!(!collapsed.contains_key("m1"));
        // ens1 should be present
        assert!(collapsed.contains_key("ens1"));
        // regular node should be present
        assert!(collapsed.contains_key("regular"));
        assert!(collapsed["ens1"].0); // is_ensemble
        assert!(!collapsed["regular"].0); // not ensemble
    }

    // ── auto_resume tests ───────────────────────────────────────────────

    #[test]
    fn sweep_orphaned_sessions_retires_orphan_rows() {
        let db = test_db();
        db.insert_interactive_session(
            "orphan-sess",
            "orphan",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.mark_session_orphaned("orphan-sess").unwrap();
        assert_eq!(db.get_orphaned_sessions().unwrap().len(), 1);

        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.sweep_orphaned_sessions();

        assert!(db.get_orphaned_sessions().unwrap().is_empty());
        assert_eq!(
            db.get_interactive_session_status("orphan-sess")
                .unwrap()
                .as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn auto_resume_skips_when_no_active_sessions() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.auto_resume_sessions();

        assert!(
            app.interactive_agents.is_empty(),
            "nothing to resume: no agent is spawned"
        );
    }
}

// ── CT14: sidebar focus/selection stale-index regressions ────────────────
// Each test covers one stale index from the CT14 design plan. They use the
// same `test_db()` + `App::new(...)` pattern as the existing sidebar tests.
#[cfg(test)]
mod ct14_sidebar_tests {
    use super::App;
    use crate::db::Database;
    use crate::domain::graphs::GraphStatus;
    use crate::tui::app::types::{
        AgentEntry, AgentSectionFocus, AutomationKind, ProjectTab, SidebarLayer,
    };
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn make_project(hash: &str, path: &str) -> crate::domain::project::Project {
        crate::domain::project::Project {
            hash: hash.to_string(),
            path: path.to_string(),
            name: hash.to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 0,
        }
    }

    fn make_graph(id: &str, name: &str) -> crate::domain::graphs::Graph {
        crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        }
    }

    fn bg_agent(id: &str) -> crate::domain::models::Agent {
        crate::domain::models::Agent {
            id: id.to_string(),
            prompt: String::new(),
            trigger: None,
            cli: crate::domain::models::Cli::new("claude"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: format!("/tmp/{id}.log"),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn history_entry(name: &str) -> crate::db::project::ProjectHistoryEntry {
        crate::db::project::ProjectHistoryEntry {
            kind: crate::db::project::ProjectHistoryKind::Graph,
            name: name.to_string(),
            status: "done".to_string(),
            at: 0,
        }
    }

    #[test]
    fn ct14_shift_arrows_cycle_after_knowledge_round_trip() {
        // FOCUS bug #1: `project_focus` must not survive leaving Knowledge —
        // a surviving focus traps Shift+←/→ in the project-tab keymap.
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.selected_project = 0;

        app.enter_project_focus(ProjectTab::Overview);
        assert_eq!(app.project_focus, Some(ProjectTab::Overview));

        // Leave Knowledge via the F2 ring walk.
        app.cycle_sidebar_layer();
        assert!(
            app.project_focus.is_none(),
            "leaving Knowledge must clear the deep project focus"
        );
        assert_ne!(
            app.sidebar_layer,
            SidebarLayer::Knowledge,
            "F2 must land on another tab"
        );

        // Shift+←/→ steps the sidebar tab ring (not the project tabs).
        let before = app.sidebar_layer;
        app.step_sidebar_tab(true);
        assert!(
            app.project_focus.is_none(),
            "tab stepping must not re-enter project focus"
        );
        assert_ne!(
            app.sidebar_layer, before,
            "Shift+→ must advance the sidebar tab ring"
        );
        app.step_sidebar_tab(false);
        assert_eq!(app.sidebar_layer, before, "Shift+← must step the ring back");
    }

    #[test]
    fn ct14_automation_kind_flips_when_its_list_empties_and_graphs_still_reachable() {
        // SELECTION bug #2: `automation_kind` pointing at an emptied sub-list
        // must flip to the non-empty side so graphs stay reachable.
        let db = test_db();
        db.insert_graph(&make_graph("graph-1", "Graph 1")).unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_graphs().expect("refresh graphs");
        app.agents = vec![AgentEntry::Agent(bg_agent("bg-1"))];
        app.automation_kind = AutomationKind::Agent;

        // The last background agent disappears while one graph exists.
        app.agents
            .retain(|a| !matches!(a, AgentEntry::Agent(_) | AgentEntry::Corrupt(_)));
        app.normalize_automation_kind();
        assert_eq!(app.automation_kind, AutomationKind::Graph);
        assert_eq!(app.selected_graph_id.as_deref(), Some("graph-1"));
        assert!(
            app.enter_layer(SidebarLayer::Automation, true),
            "graphs section must stay reachable"
        );

        // Symmetric subcase: graphs vanish while agents remain.
        app.graphs.clear();
        app.archived_graphs.clear();
        app.selected_graph_id = Some("graph-1".to_string());
        app.automation_kind = AutomationKind::Graph;
        app.agents = vec![AgentEntry::Agent(bg_agent("bg-2"))];
        app.selected = 99;
        app.normalize_automation_kind();
        assert_eq!(app.automation_kind, AutomationKind::Agent);
        assert!(
            app.automation_agent_indices().contains(&app.selected),
            "agent cursor must name a live row"
        );
        assert!(
            app.enter_layer(SidebarLayer::Automation, true),
            "agents side must stay reachable"
        );
    }

    #[test]
    fn ct14_agent_section_focus_moves_off_empty_section() {
        // SELECTION bug #3 (C32 follow-on): a zero-row section must not keep
        // the focus that `fair_section_heights` turns into a floor.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.agents = vec![AgentEntry::Terminal(0)];
        app.agent_section_focus = AgentSectionFocus::Terminal;
        app.agents = vec![AgentEntry::Interactive(0)];
        app.normalize_agent_section_focus();
        assert_eq!(app.agent_section_focus, AgentSectionFocus::Interactive);

        // Subcase: only Groups remain.
        app.split_groups.push(crate::domain::models::SplitGroup {
            id: "g0".to_string(),
            orientation: crate::domain::models::SplitOrientation::Horizontal,
            session_a: "a".to_string(),
            session_b: "b".to_string(),
            created_at: chrono::Utc::now(),
        });
        app.agents = vec![AgentEntry::Group(0)];
        app.agent_section_focus = AgentSectionFocus::Terminal;
        app.normalize_agent_section_focus();
        assert_eq!(app.agent_section_focus, AgentSectionFocus::Groups);
    }

    #[test]
    fn ct14_selected_and_scroll_clamped_after_shrink() {
        // SELECTION bugs #4/#5/#7: every stored cursor is corrected at the
        // moment of use instead of failing silently.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // `selected` past the end of a shrunk agent list.
        app.agents = vec![
            AgentEntry::Group(0),
            AgentEntry::Group(1),
            AgentEntry::Group(2),
            AgentEntry::Group(3),
            AgentEntry::Group(4),
        ];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 4;
        app.agents.truncate(3);
        app.clamp_sidebar_selection();
        assert_eq!(app.selected, 2);

        // `selected_project` past the end of a shrunk project list.
        app.projects = vec![
            make_project("h0", "/tmp/a"),
            make_project("h1", "/tmp/b"),
            make_project("h2", "/tmp/c"),
        ];
        app.selected_project = 2;
        app.projects.truncate(1);
        app.clamp_sidebar_selection();
        assert_eq!(app.selected_project, 0);

        // History cursor belongs to the previous project's longer list.
        app.projects = vec![make_project("h0", "/tmp/a"), make_project("h1", "/tmp/b")];
        app.project_history_cache.insert(
            "h0".to_string(),
            (0..6).map(|i| history_entry(&format!("e{i}"))).collect(),
        );
        app.project_history_cache
            .insert("h1".to_string(), vec![history_entry("only")]);
        app.selected_project = 0;
        app.selected_project_history = 5;
        app.project_focus = Some(ProjectTab::History);
        app.selected_project = 1;
        app.clamp_sidebar_selection();
        assert_eq!(app.selected_project_history, 0);

        // Scroll offset stranded past the new tab total.
        app.project_focus = None;
        app.sidebar_layer = SidebarLayer::Live;
        app.sidebar_visible_capacity = 5;
        app.sidebar_scroll_offset = 10;
        app.agents = vec![AgentEntry::Group(0), AgentEntry::Group(1)];
        app.selected = 0;
        app.clamp_sidebar_selection();
        assert_eq!(app.sidebar_scroll_offset, 0);

        // Arrow navigation stays within the new bounds.
        let live: Vec<usize> = app.live_indices();
        app.select_next();
        assert!(
            live.contains(&app.selected),
            "select_next must land on an existing row"
        );
        app.select_prev();
        assert!(
            live.contains(&app.selected),
            "select_prev must land on an existing row"
        );
    }

    #[test]
    fn ct14_tab_memory_dead_slot_cleared_on_failed_restore() {
        // SELECTION bug #6: a dead `SidebarStepMemory` slot must be cleared on
        // failed restore, not re-probed on every revisit.
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Group(0)];

        app.sidebar_step_memory.live_selected = Some(99);
        assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Live));
        assert_eq!(app.sidebar_step_memory.live_selected, None);

        app.sidebar_step_memory.automation_kind = Some(AutomationKind::Graph);
        app.sidebar_step_memory.automation_graph_id = Some("ghost".to_string());
        assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Automation));
        assert_eq!(app.sidebar_step_memory.automation_graph_id, None);

        app.sidebar_step_memory.knowledge_selected = Some(7);
        assert!(!app.restore_remembered_sidebar_selection(SidebarLayer::Knowledge));
        assert_eq!(app.sidebar_step_memory.knowledge_selected, None);
    }

    #[test]
    fn ct14_long_scripted_sequence_leaves_navigation_working() {
        // The spec's scripted reproduction: start Live → enter Knowledge →
        // move within it → leave → F2 cycle → Shift+←/→ both ways → enter
        // Automation → navigate both ways → shrink graphs between ticks →
        // navigate again. Navigation must work throughout.
        let db = test_db();
        db.upsert_project(&make_project("hash0", "/tmp/proj0"))
            .unwrap();
        db.insert_graph(&make_graph("graph-1", "Graph 1")).unwrap();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.refresh_graphs().expect("refresh graphs");
        app.agents = vec![
            AgentEntry::Group(0),
            AgentEntry::Group(1),
            AgentEntry::Agent(bg_agent("bg-1")),
        ];
        app.sidebar_layer = SidebarLayer::Live;
        app.selected = 0;

        // Enter Knowledge, move within the project tabs, leave via Esc.
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.selected_project = 0;
        app.enter_project_focus(ProjectTab::Overview);
        app.navigate_project_tab_list(true);
        app.cycle_project_tab(true);
        assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
        app.navigate_project_tab_list(true);
        app.exit_project_focus();
        assert!(app.project_focus.is_none());

        // Switch sections a few times (F2) and cycle tabs (Shift+←/→).
        app.cycle_sidebar_layer();
        app.cycle_sidebar_layer();
        let ring_pos = app.sidebar_layer;
        app.step_sidebar_tab(true);
        app.step_sidebar_tab(false);
        assert_eq!(app.sidebar_layer, ring_pos);
        assert!(app.project_focus.is_none());

        // Reach the graphs section and move within Automation.
        app.switch_sidebar_tab(SidebarLayer::Automation);
        assert_eq!(app.sidebar_layer, SidebarLayer::Automation);
        app.select_next();
        app.select_prev();

        // Shrink the graphs list between ticks, then navigate again.
        app.graphs.clear();
        app.archived_graphs.clear();
        app.refresh_graphs_selection();
        app.normalize_automation_kind();
        app.normalize_agent_section_focus();
        app.clamp_sidebar_selection();
        app.select_next();
        app.select_prev();

        // Every cursor still names something that exists.
        match app.sidebar_layer {
            SidebarLayer::Live => {
                assert!(app.live_indices().contains(&app.selected));
            }
            SidebarLayer::Automation => match app.automation_kind {
                AutomationKind::Agent => {
                    assert!(app.automation_agent_indices().contains(&app.selected));
                }
                AutomationKind::Graph => {
                    let id = app.selected_graph_id.clone().expect("graph cursor set");
                    assert!(app.sidebar_graphs().iter().any(|lp| lp.id == id));
                }
            },
            SidebarLayer::Knowledge => {
                assert!(app.selected_project < app.projects.len());
            }
        }

        // Shift+←/→ still cycles the tab ring.
        let before = app.sidebar_layer;
        app.step_sidebar_tab(true);
        assert_ne!(app.sidebar_layer, before);
        assert!(app.project_focus.is_none());
    }
}
