//! App sidebar nav methods moved from the composition root.

use super::*;

impl App {
    pub fn select_next(&mut self) {
        if self.agents_rag_focused {
            self.leave_rag_focus();
            self.reset_log_scroll();
            return;
        }
        if self.project_focus.is_some() {
            self.navigate_project_tab_list(true);
            self.reset_log_scroll();
            return;
        }
        match self.sidebar_layer {
            SidebarLayer::Live => self.navigate_live(true),
            SidebarLayer::Automation => self.navigate_automation(true),
            SidebarLayer::Knowledge => self.navigate_projects_next(),
        }
        self.reset_log_scroll();
    }

    pub fn select_prev(&mut self) {
        if self.agents_rag_focused {
            self.leave_rag_focus();
            self.reset_log_scroll();
            return;
        }
        if self.project_focus.is_some() {
            self.navigate_project_tab_list(false);
            self.reset_log_scroll();
            return;
        }
        match self.sidebar_layer {
            SidebarLayer::Live => self.navigate_live(false),
            SidebarLayer::Automation => self.navigate_automation(false),
            SidebarLayer::Knowledge => self.navigate_projects_prev(),
        }
        self.reset_log_scroll();
    }

    /// Indices into `app.agents` that render inside the `Live` layer
    /// (interactive sessions, terminals, orphaned sessions, split groups —
    /// everything with a PTY right now), in rendering order.
    pub(crate) fn live_indices(&self) -> Vec<usize> {
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                matches!(
                    a,
                    AgentEntry::Interactive(_)
                        | AgentEntry::Terminal(_)
                        | AgentEntry::Orphaned(_)
                        | AgentEntry::Group(_)
                )
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Indices into `app.agents` that render inside the `Automation` layer's
    /// agent sub-list (background agents, including corrupt rows).
    pub(crate) fn automation_agent_indices(&self) -> Vec<usize> {
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, a)| matches!(a, AgentEntry::Agent(_) | AgentEntry::Corrupt(_)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Ordered index set that the in-focus `Shift+Up`/`Shift+Down` cycle
    /// (`agent_focus::handle_agent_cycle_shortcut`) walks — the same set the
    /// active tab's own arrow navigation shows, so cycling can never land on an
    /// entry rendered under a different tab (CT20). Knowledge has no live
    /// sessions to focus into, so it contributes nothing.
    pub(crate) fn cycle_focus_indices(&self) -> Vec<usize> {
        match self.sidebar_layer {
            SidebarLayer::Live => self.live_indices(),
            SidebarLayer::Automation => self.automation_agent_indices(),
            SidebarLayer::Knowledge => Vec::new(),
        }
    }

    /// Arrows never change the active tab (decisions 1–2): running off
    /// either end of `Live`'s own list wraps within it. The one exception is
    /// backing off the first item, which instead focuses the pinned RAG
    /// summary above the tab bar when it has anything to show (decision 3);
    /// with nothing to show there, that end wraps to the last item too.
    pub(crate) fn navigate_live(&mut self, forward: bool) {
        // CT14 (FR5): row count is recomputed from current data at the moment
        // of use; a stale cursor outside the current indices is corrected
        // before moving, not left to fail silently.
        self.normalize_agent_section_focus();
        let indices = self.live_indices();
        if indices.is_empty() {
            return;
        }
        let current = indices.iter().position(|&i| i == self.selected);
        let Some(pos) = current else {
            let prev = self.selected;
            self.selected = indices[0];
            self.update_agent_section_focus_on_change(prev);
            return;
        };
        let next_pos = if forward {
            if pos + 1 < indices.len() {
                pos + 1
            } else {
                0
            }
        } else if pos > 0 {
            pos - 1
        } else if self.rag_info.has_rag_activity() {
            self.enter_rag_focus();
            return;
        } else {
            indices.len() - 1
        };
        let prev = self.selected;
        self.selected = indices[next_pos];
        self.update_agent_section_focus_on_change(prev);
    }

    /// Automation is one flat ring for arrow-key purposes, agents rendered
    /// above graphs: `[agent, agent, …, graph, graph, …]`. Running off either
    /// end wraps within that same flat list (decisions 1–2), except backing
    /// off the very first item, which focuses the pinned RAG summary when it
    /// has anything to show (decision 3) and otherwise wraps to the last
    /// entry like any other tab.
    pub(crate) fn navigate_automation(&mut self, forward: bool) {
        // CT14 (FR5): recompute the row count from current data at the moment
        // of use, and correct a stale kind pointing at an emptied sub-list.
        self.normalize_automation_kind();
        let agent_indices = self.automation_agent_indices();
        let graph_ids: Vec<String> = self
            .sidebar_graphs()
            .into_iter()
            .map(|lp| lp.id.clone())
            .collect();
        let total = agent_indices.len() + graph_ids.len();
        if total == 0 {
            return;
        }

        let current = match self.automation_kind {
            AutomationKind::Agent => agent_indices.iter().position(|&i| i == self.selected),
            AutomationKind::Graph => self
                .selected_graph_id
                .as_ref()
                .and_then(|id| graph_ids.iter().position(|v| v == id))
                .map(|pos| agent_indices.len() + pos),
        };
        let target_pos = match current {
            Some(pos) if forward && pos + 1 < total => Some(pos + 1),
            Some(_) if forward => Some(0),
            Some(pos) if pos > 0 => Some(pos - 1),
            Some(_) => None,
            None => Some(if forward { 0 } else { total - 1 }),
        };
        let pos = match target_pos {
            Some(pos) => pos,
            None => {
                if self.rag_info.has_rag_activity() {
                    self.enter_rag_focus();
                    return;
                }
                total - 1
            }
        };

        if pos < agent_indices.len() {
            self.automation_kind = AutomationKind::Agent;
            let prev = self.selected;
            self.selected = agent_indices[pos];
            self.update_agent_section_focus_on_change(prev);
        } else {
            self.automation_kind = AutomationKind::Graph;
            self.selected_graph_id = Some(graph_ids[pos - agent_indices.len()].clone());
            self.refresh_graphs_selection();
        }
    }

    pub(crate) fn navigate_projects_next(&mut self) {
        let visible = self.visible_project_indices();
        if visible.is_empty() {
            return;
        }
        let pos = visible.iter().position(|&idx| idx == self.selected_project);
        self.selected_project = match pos {
            Some(pos) => visible[(pos + 1) % visible.len()],
            None => visible[0],
        };
        self.refresh_graphs_selection();
    }

    pub(crate) fn navigate_projects_prev(&mut self) {
        let visible = self.visible_project_indices();
        if visible.is_empty() {
            return;
        }
        match visible.iter().position(|&idx| idx == self.selected_project) {
            Some(pos) if pos > 0 => self.selected_project = visible[pos - 1],
            // Off the top: leave for the RAG summary when there is one and no
            // type-ahead is narrowing the list — a filtered list owns its
            // edges (the wrap stays inside the matches).
            _ if self.project_filter.is_empty() && self.rag_info.has_rag_activity() => {
                self.enter_rag_focus();
                return;
            }
            _ => self.selected_project = *visible.last().unwrap(),
        }
        self.refresh_graphs_selection();
    }

    /// Try focusing the first/last navigable item of `layer`. Returns
    /// `false` (and touches nothing) if `layer` has nothing to select, so
    /// callers that walk multiple layers (`focus_sidebar_from_edge`,
    /// `cycle_sidebar_layer`) can keep looking.
    pub(crate) fn enter_layer(&mut self, layer: SidebarLayer, forward: bool) -> bool {
        // CT14 (focus bug): leaving Knowledge abandons the deep project view;
        // a surviving `project_focus` would trap Shift+arrows in the
        // project-tab keymap on return.
        if self.sidebar_layer == SidebarLayer::Knowledge
            && layer != SidebarLayer::Knowledge
            && self.project_focus.is_some()
        {
            self.exit_project_focus();
        }
        match layer {
            SidebarLayer::Live => {
                self.normalize_agent_section_focus();
                self.clamp_sidebar_selection();
                let indices = self.live_indices();
                if indices.is_empty() {
                    return false;
                }
                self.sidebar_layer = SidebarLayer::Live;
                let prev = self.selected;
                self.selected = if forward {
                    indices[0]
                } else {
                    *indices.last().unwrap()
                };
                self.update_agent_section_focus_on_change(prev);
                true
            }
            SidebarLayer::Automation => {
                self.normalize_automation_kind();
                self.clamp_sidebar_selection();
                let agent_indices = self.automation_agent_indices();
                let graph_ids: Vec<String> = self
                    .sidebar_graphs()
                    .into_iter()
                    .map(|lp| lp.id.clone())
                    .collect();
                if agent_indices.is_empty() && graph_ids.is_empty() {
                    return false;
                }
                self.sidebar_layer = SidebarLayer::Automation;
                // Agents render above graphs, so entering forward (from
                // above) lands on agents first; entering backward (from
                // below) lands on graphs first — whichever list is empty is
                // skipped.
                if forward {
                    if !agent_indices.is_empty() {
                        self.automation_kind = AutomationKind::Agent;
                        let prev = self.selected;
                        self.selected = agent_indices[0];
                        self.update_agent_section_focus_on_change(prev);
                    } else {
                        self.automation_kind = AutomationKind::Graph;
                        self.selected_graph_id = Some(graph_ids[0].clone());
                        self.refresh_graphs_selection();
                    }
                } else if !graph_ids.is_empty() {
                    self.automation_kind = AutomationKind::Graph;
                    self.selected_graph_id = Some(graph_ids.last().unwrap().clone());
                    self.refresh_graphs_selection();
                } else {
                    self.automation_kind = AutomationKind::Agent;
                    let prev = self.selected;
                    self.selected = *agent_indices.last().unwrap();
                    self.update_agent_section_focus_on_change(prev);
                }
                true
            }
            SidebarLayer::Knowledge => {
                if self.projects.is_empty() {
                    return false;
                }
                self.sidebar_layer = SidebarLayer::Knowledge;
                self.selected_project = if forward { 0 } else { self.projects.len() - 1 };
                self.refresh_graphs_selection();
                true
            }
        }
    }

    pub(crate) fn enter_rag_focus(&mut self) {
        self.agents_rag_focused = true;
    }

    /// Leaving the pinned RAG summary: entering RAG focus never touches
    /// `sidebar_layer` (decision 1 — arrows never change tabs), so leaving it
    /// always returns to the first item of whichever tab was active when RAG
    /// was entered (decision 3), regardless of direction.
    pub(crate) fn leave_rag_focus(&mut self) {
        self.agents_rag_focused = false;
        self.enter_layer(self.sidebar_layer, true);
    }

    /// Move a project's active `ProjectTab` list selection. Arrows never
    /// change tabs (functional requirement 4) — only the list inside the
    /// current tab moves, or wraps within it.
    pub(crate) fn navigate_project_tab_list(&mut self, forward: bool) {
        match self.project_focus {
            Some(ProjectTab::Overview) | None => {}
            Some(ProjectTab::Backlog) => {
                if self.backlog_specs.is_empty() {
                    return;
                }
                self.selected_backlog = crate::tui::selection::move_index(
                    self.selected_backlog,
                    self.backlog_specs.len(),
                    forward,
                );
            }
            Some(ProjectTab::Knowledge) => {
                if forward {
                    self.navigate_knowledge_next();
                } else {
                    self.navigate_knowledge_prev();
                }
            }
            Some(ProjectTab::History) => {
                let len = self.selected_project_history_entries().len();
                if len == 0 {
                    return;
                }
                self.selected_project_history =
                    crate::tui::selection::move_index(self.selected_project_history, len, forward);
            }
        }
    }

    pub(crate) fn navigate_knowledge_next(&mut self) {
        let filtered = self.filtered_knowledge_indices();
        if filtered.is_empty() {
            return;
        }
        let current = filtered
            .iter()
            .position(|&idx| idx == self.selected_knowledge)
            .unwrap_or(0);
        let next = crate::tui::selection::move_index(current, filtered.len(), true);
        self.selected_knowledge = filtered[next];
    }

    pub(crate) fn navigate_knowledge_prev(&mut self) {
        let filtered = self.filtered_knowledge_indices();
        if filtered.is_empty() {
            return;
        }
        let current = filtered
            .iter()
            .position(|&idx| idx == self.selected_knowledge)
            .unwrap_or(0);
        let next = crate::tui::selection::move_index(current, filtered.len(), false);
        self.selected_knowledge = filtered[next];
    }

    pub(crate) fn update_agent_section_focus_on_change(&mut self, _prev_selected: usize) {
        if let Some(agent) = self.agents.get(self.selected) {
            match agent {
                AgentEntry::Agent(_) | AgentEntry::Corrupt(_) => {
                    self.sidebar_layer = SidebarLayer::Automation;
                    self.automation_kind = AutomationKind::Agent;
                }
                AgentEntry::Interactive(_) | AgentEntry::Orphaned(_) => {
                    self.sidebar_layer = SidebarLayer::Live;
                    self.agent_section_focus = AgentSectionFocus::Interactive;
                }
                AgentEntry::Terminal(_) => {
                    self.sidebar_layer = SidebarLayer::Live;
                    self.agent_section_focus = AgentSectionFocus::Terminal;
                }
                AgentEntry::Group(_) => {
                    self.sidebar_layer = SidebarLayer::Live;
                    self.agent_section_focus = AgentSectionFocus::Groups;
                }
            };
        }
    }

    /// CT14: correct (never discard) stored sidebar state at the moment of
    /// use. Each normalizer below prefers the remembered value and only moves
    /// it when it no longer points at something that exists — this preserves
    /// the user's place per the spec constraint (no reset-to-default).
    pub(crate) fn clamp_sidebar_selection(&mut self) {
        if !self.agents.is_empty() && self.selected >= self.agents.len() {
            self.selected = self.agents.len() - 1;
        }
        if !self.projects.is_empty() && self.selected_project >= self.projects.len() {
            self.selected_project = self.projects.len() - 1;
        }
        if self.projects.is_empty() {
            self.selected_project = 0;
        }
        // Clamp the Knowledge History-tab cursor to the currently selected
        // project's entries (moment of use, FR5).
        if self.project_focus == Some(ProjectTab::History) {
            let len = self.selected_project_history_entries().len();
            if len == 0 {
                self.selected_project_history = 0;
            } else if self.selected_project_history >= len {
                self.selected_project_history = len - 1;
            }
        }
        // Clamp the scroll offset against the current layer's row count so a
        // capacity change across tab switches can't strand it past the end.
        let total = match self.sidebar_layer {
            SidebarLayer::Live => self.live_indices().len(),
            SidebarLayer::Automation => {
                self.automation_agent_indices().len() + self.sidebar_graphs().len()
            }
            SidebarLayer::Knowledge => self.projects.len(),
        };
        let max_offset = total.saturating_sub(self.sidebar_visible_capacity);
        if self.sidebar_scroll_offset > max_offset {
            self.sidebar_scroll_offset = max_offset;
        }
    }

    /// CT14 (selection bug): if `automation_kind` points at an empty sub-list
    /// while the other one has rows, flip it to the non-empty side. Pure
    /// state, no redraw.
    pub(crate) fn normalize_automation_kind(&mut self) {
        let has_agents = !self.automation_agent_indices().is_empty();
        let has_graphs = !self.sidebar_graphs().is_empty();
        match self.automation_kind {
            AutomationKind::Agent if !has_agents && has_graphs => {
                self.automation_kind = AutomationKind::Graph;
                // Keep the graph cursor valid without recursing into
                // `refresh_graphs_selection` (which itself calls this
                // normalizer — see its tail).
                let valid = self
                    .selected_graph_id
                    .as_deref()
                    .is_some_and(|id| self.sidebar_graphs().iter().any(|lp| lp.id == id));
                if !valid {
                    self.selected_graph_id = self.sidebar_graphs().first().map(|lp| lp.id.clone());
                }
            }
            AutomationKind::Graph if !has_graphs && has_agents => {
                self.automation_kind = AutomationKind::Agent;
                if !self.automation_agent_indices().contains(&self.selected) {
                    let prev = self.selected;
                    self.selected = self.automation_agent_indices()[0];
                    self.update_agent_section_focus_on_change(prev);
                }
            }
            _ => {}
        }
    }

    /// CT14 (selection bug): if `agent_section_focus` points at an empty Live
    /// sub-list, move it to the first non-empty one. Pure state, no redraw.
    pub(crate) fn normalize_agent_section_focus(&mut self) {
        let has_interactive = self
            .agents
            .iter()
            .any(|a| matches!(a, AgentEntry::Interactive(_) | AgentEntry::Orphaned(_)));
        let has_terminal = self
            .agents
            .iter()
            .any(|a| matches!(a, AgentEntry::Terminal(_)));
        let has_groups = !self.split_groups.is_empty();
        let empty = match self.agent_section_focus {
            AgentSectionFocus::Interactive => !has_interactive,
            AgentSectionFocus::Terminal => !has_terminal,
            AgentSectionFocus::Groups => !has_groups,
            AgentSectionFocus::Brain => false,
        };
        if !empty {
            return;
        }
        if has_interactive {
            self.agent_section_focus = AgentSectionFocus::Interactive;
        } else if has_terminal {
            self.agent_section_focus = AgentSectionFocus::Terminal;
        } else if has_groups {
            self.agent_section_focus = AgentSectionFocus::Groups;
        }
    }

    pub(crate) fn reset_log_scroll(&mut self) {
        self.log_scroll = 0;
        self.sidebar_scroll_offset = 0;
    }

    /// Select an agent by its flat index into `self.agents`, mirroring the
    /// bookkeeping arrow-key navigation performs (section focus, RAG focus,
    /// scroll reset). Used by sidebar mouse clicks.
    pub(crate) fn select_agent_at(&mut self, idx: usize) {
        if idx >= self.agents.len() {
            return;
        }
        let prev = self.selected;
        self.selected = idx;
        self.agents_rag_focused = false;
        self.update_agent_section_focus_on_change(prev);
        self.reset_log_scroll();
    }

    pub fn scroll_log_down(&mut self) {
        self.log_scroll = self.log_scroll.saturating_add(3);
    }

    pub fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(3);
    }
}
