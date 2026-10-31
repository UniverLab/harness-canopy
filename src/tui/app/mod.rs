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
pub(crate) mod hub_remote;
pub(crate) mod hub_remote_trust;
pub(crate) mod hub_settings;
pub(crate) mod hub_shelf;
mod init;
pub(crate) mod panel_face;
mod project_activity;
mod project_filter;
mod project_graph;
mod project_nav;
mod refresh;
mod refresh_cadence;
mod refresh_graph_whimsg;
pub(crate) mod session_hub;
mod sessions;
mod sidebar_nav;
mod slow_fingerprint;
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
pub(crate) use slow_fingerprint::SlowFingerprint;
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
mod tests;

// ── CT14: sidebar focus/selection stale-index regressions ────────────────
// Each test covers one stale index from the CT14 design plan. They use the
// same `test_db()` + `App::new(...)` pattern as the existing sidebar tests.
#[cfg(test)]
mod ct14_sidebar_tests;
