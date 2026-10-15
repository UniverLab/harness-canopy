//! App project nav methods moved from the composition root.

use super::*;

impl App {
    pub(crate) fn focus_sidebar_from_edge(&mut self, from_top: bool) {
        if from_top {
            if self.rag_info.has_rag_activity() {
                self.enter_rag_focus();
                return;
            }
            for layer in [
                SidebarLayer::Live,
                SidebarLayer::Automation,
                SidebarLayer::Knowledge,
            ] {
                if self.enter_layer(layer, true) {
                    return;
                }
            }
        } else {
            for layer in [
                SidebarLayer::Knowledge,
                SidebarLayer::Automation,
                SidebarLayer::Live,
            ] {
                if self.enter_layer(layer, false) {
                    return;
                }
            }
            if self.rag_info.has_rag_activity() {
                self.enter_rag_focus();
            }
        }
    }

    /// Jump directly to the next sidebar tab (F2 / right-click), skipping
    /// tabs with nothing to select — a keyboard-only shortcut alongside
    /// arrow-key ring navigation.
    pub(crate) fn cycle_sidebar_layer(&mut self) {
        // CT14 (focus bug): departing Knowledge must not leave a dangling
        // `project_focus` behind — it traps Shift+arrows in the project-tab
        // keymap after returning to another layer.
        if self.sidebar_layer == SidebarLayer::Knowledge && self.project_focus.is_some() {
            self.exit_project_focus();
        }
        self.normalize_automation_kind();
        self.agents_rag_focused = false;
        let ring = Self::SIDEBAR_TAB_RING;
        let start_idx = Self::sidebar_tab_index(self.sidebar_layer);
        for step in 1..=ring.len() {
            let layer = ring[(start_idx + step) % ring.len()];
            if self.enter_layer(layer, true) {
                return;
            }
        }
    }

    /// Left-to-right order of the sidebar tab strip. Mirrors `SIDEBAR_TABS`
    /// in the renderer, which is what Shift+←/→ has to agree with for the
    /// arrows to move the way the strip looks.
    const SIDEBAR_TAB_RING: [SidebarLayer; 3] = [
        SidebarLayer::Live,
        SidebarLayer::Automation,
        SidebarLayer::Knowledge,
    ];

    pub(crate) fn sidebar_tab_index(layer: SidebarLayer) -> usize {
        Self::SIDEBAR_TAB_RING
            .iter()
            .position(|&l| l == layer)
            .unwrap_or(0)
    }

    /// Shift+←/→ — move exactly one tab in `forward`'s direction, wrapping
    /// at the ends. Deliberately does NOT skip empty tabs the way F2 does:
    /// a directional key that silently jumps two cells because the one in
    /// between was empty reads as a bug, and the empty tab's own state is
    /// worth seeing. Now reachable from `Focus::Agent` too (not just
    /// Home/Preview) when no split is active — see the guard in
    /// `event::handle_global_key` — so a step away from a layer remembers
    /// that layer's selection and a step back restores it instead of
    /// re-landing on its edge item the way a fresh jump (click/F2) does.
    pub(crate) fn step_sidebar_tab(&mut self, forward: bool) {
        // CT14: correct stale indices at the moment of use (FR4/FR5) before
        // stepping, so tab cycling works from any reachable state.
        self.clamp_sidebar_selection();
        let ring = Self::SIDEBAR_TAB_RING;
        let idx = Self::sidebar_tab_index(self.sidebar_layer);
        let next = crate::tui::selection::move_index(idx, ring.len(), forward);
        self.remember_current_sidebar_selection();
        let target = ring[next];
        // CT14 (focus bug): leaving Knowledge clears the deep project view.
        if self.sidebar_layer == SidebarLayer::Knowledge
            && target != SidebarLayer::Knowledge
            && self.project_focus.is_some()
        {
            self.exit_project_focus();
        }
        self.agents_rag_focused = false;
        if !self.restore_remembered_sidebar_selection(target) {
            self.switch_sidebar_tab(target);
        }
    }

    /// Snapshot the outgoing sidebar layer's own selection into
    /// `sidebar_step_memory` before `step_sidebar_tab` moves off it. `Live`
    /// and Automation's agent sub-list share `selected` as their index
    /// space, so leaving one and entering the other would otherwise
    /// overwrite the value the departing layer needs back.
    pub(crate) fn remember_current_sidebar_selection(&mut self) {
        match self.sidebar_layer {
            SidebarLayer::Live => {
                self.sidebar_step_memory.live_selected = Some(self.selected);
            }
            SidebarLayer::Automation => {
                self.sidebar_step_memory.automation_kind = Some(self.automation_kind);
                // Only the active branch's cursor is refreshed; the other
                // branch's slot is left alone so a step back restores the
                // kind that was actually last used.
                match self.automation_kind {
                    AutomationKind::Agent => {
                        self.sidebar_step_memory.automation_selected = Some(self.selected);
                    }
                    AutomationKind::Graph => {
                        self.sidebar_step_memory.automation_graph_id =
                            self.selected_graph_id.clone();
                    }
                }
            }
            SidebarLayer::Knowledge => {
                self.sidebar_step_memory.knowledge_selected = Some(self.selected_project);
            }
        }
    }

    /// Try to restore `layer`'s remembered selection (still valid against
    /// current data); returns `false` if nothing was remembered or it no
    /// longer applies, so the caller falls back to the edge-jump `enter_layer`
    /// uses for a fresh jump.
    pub(crate) fn restore_remembered_sidebar_selection(&mut self, layer: SidebarLayer) -> bool {
        match layer {
            SidebarLayer::Live => {
                let Some(idx) = self.sidebar_step_memory.live_selected else {
                    return false;
                };
                if !self.live_indices().contains(&idx) {
                    // CT14: stale memory must not be re-probed forever —
                    // clear the dead slot so the next revisit goes straight
                    // to the edge item.
                    self.sidebar_step_memory.live_selected = None;
                    return false;
                }
                self.sidebar_layer = SidebarLayer::Live;
                self.selected = idx;
                true
            }
            SidebarLayer::Automation => match self.sidebar_step_memory.automation_kind {
                Some(AutomationKind::Agent) => {
                    let Some(idx) = self.sidebar_step_memory.automation_selected else {
                        return false;
                    };
                    if !self.automation_agent_indices().contains(&idx) {
                        // CT14: clear the dead slot (see Live branch above).
                        self.sidebar_step_memory.automation_selected = None;
                        return false;
                    }
                    self.sidebar_layer = SidebarLayer::Automation;
                    self.automation_kind = AutomationKind::Agent;
                    self.selected = idx;
                    true
                }
                Some(AutomationKind::Graph) => {
                    let Some(id) = self.sidebar_step_memory.automation_graph_id.clone() else {
                        return false;
                    };
                    if !self.sidebar_graphs().iter().any(|lp| lp.id == id) {
                        // CT14: clear the dead slot (see Live branch above).
                        self.sidebar_step_memory.automation_graph_id = None;
                        return false;
                    }
                    self.sidebar_layer = SidebarLayer::Automation;
                    self.automation_kind = AutomationKind::Graph;
                    self.selected_graph_id = Some(id);
                    self.refresh_graphs_selection();
                    true
                }
                None => false,
            },
            SidebarLayer::Knowledge => {
                let Some(idx) = self.sidebar_step_memory.knowledge_selected else {
                    return false;
                };
                if idx >= self.projects.len() {
                    // CT14: clear the dead slot (see Live branch above).
                    self.sidebar_step_memory.knowledge_selected = None;
                    return false;
                }
                self.sidebar_layer = SidebarLayer::Knowledge;
                self.selected_project = idx;
                true
            }
        }
    }

    /// Enter a highlighted project's Focus tab bar (functional requirement
    /// 4). `tab` defaults to `Overview`; History lazily loads on first show.
    pub(crate) fn enter_project_focus(&mut self, tab: ProjectTab) {
        self.project_focus = Some(tab);
        if tab == ProjectTab::History {
            let _ = self.selected_project_history_entries();
        }
    }

    /// Leave a project's Focus tab bar back to the sidebar (Esc).
    pub(crate) fn exit_project_focus(&mut self) {
        self.project_focus = None;
    }

    /// Tab/Shift+Tab, `]`/`[`, or Shift+←/→ inside a project's Focus tab bar.
    pub(crate) fn cycle_project_tab(&mut self, forward: bool) {
        let Some(current) = self.project_focus else {
            return;
        };
        let idx = ProjectTab::ALL
            .iter()
            .position(|&t| t == current)
            .unwrap_or(0);
        let next = crate::tui::selection::move_index(idx, ProjectTab::ALL.len(), forward);
        self.enter_project_focus(ProjectTab::ALL[next]);
    }

    /// Direct hotkey (o/b/k/h) to jump straight to a tab.
    pub(crate) fn open_project_tab(&mut self, tab: ProjectTab) {
        self.enter_project_focus(tab);
    }

    /// Mouse click on the active tab's list at display row `idx` — sets the
    /// tab's selection directly (unlike arrow keys, which move by one).
    pub(crate) fn set_project_tab_row(&mut self, idx: usize) {
        match self.project_focus {
            Some(ProjectTab::Backlog) => {
                if idx < self.backlog_specs.len() {
                    self.selected_backlog = idx;
                }
            }
            Some(ProjectTab::Knowledge) => {
                if let Some(&node_idx) = self.filtered_knowledge_indices().get(idx) {
                    self.selected_knowledge = node_idx;
                }
            }
            Some(ProjectTab::History) => {
                if idx < self.selected_project_history_entries().len() {
                    self.selected_project_history = idx;
                }
            }
            Some(ProjectTab::Overview) | None => {}
        }
    }

    /// Mouse click on a sidebar tab label — switches straight to it,
    /// entering its first item when it has one (consistent with
    /// `cycle_sidebar_layer`'s keyboard behavior). Unlike the keyboard
    /// cycle, this also switches into a tab with nothing to select, since a
    /// deliberate click on a visible tab must always land there — a click on
    /// an empty Automation should show its empty state, not silently no-op.
    pub(crate) fn switch_sidebar_tab(&mut self, layer: SidebarLayer) {
        // CT14 (focus bug): direct jumps off Knowledge also leave the deep
        // project view.
        if self.sidebar_layer == SidebarLayer::Knowledge
            && layer != SidebarLayer::Knowledge
            && self.project_focus.is_some()
        {
            self.exit_project_focus();
        }
        self.agents_rag_focused = false;
        if !self.enter_layer(layer, true) {
            self.sidebar_layer = layer;
        }
    }
}
