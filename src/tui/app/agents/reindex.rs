use crate::tui::app::types::{AgentEntry, App, Focus};

impl App {
    /// Rebuild the sidebar's session-derived entries from the live in-memory
    /// session lists so `agents`, `selected`, and split-group state never hold
    /// a stale index. Background-agent and corrupt rows are kept as the prefix;
    /// this is DB-free and safe to call from the hub dispatch tick.
    pub(crate) fn reindex_sessions(&mut self) {
        self.agents
            .retain(|e| matches!(e, AgentEntry::Agent(_) | AgentEntry::Corrupt(_)));

        for i in 0..self.interactive_agents.len() {
            self.agents.push(AgentEntry::Interactive(i));
        }
        for i in 0..self.terminal_agents.len() {
            self.agents.push(AgentEntry::Terminal(i));
        }
        for i in 0..self.orphaned_sessions.len() {
            self.agents.push(AgentEntry::Orphaned(i));
        }

        let live_names: std::collections::HashSet<&str> = self
            .interactive_agents
            .iter()
            .map(|a| a.name.as_str())
            .chain(self.terminal_agents.iter().map(|a| a.name.as_str()))
            .collect();
        self.split_groups.retain(|g| {
            live_names.contains(g.session_a.as_str()) && live_names.contains(g.session_b.as_str())
        });
        if let Some(active) = self.active_split_id.as_deref() {
            if !self.split_groups.iter().any(|g| g.id == active) {
                self.active_split_id = None;
            }
        }
        for i in 0..self.split_groups.len() {
            self.agents.push(AgentEntry::Group(i));
        }

        self.clamp_live_section_offsets();

        let total = self.agents.len();
        if total == 0 {
            self.selected = 0;
            if self.focus == Focus::Agent {
                self.focus = Focus::Preview;
            }
        } else {
            if self.selected >= total {
                self.selected = total - 1;
            }
            if self.focus == Focus::Agent {
                match self.agents.get(self.selected) {
                    Some(AgentEntry::Agent(_))
                    | Some(AgentEntry::Corrupt(_))
                    | Some(AgentEntry::Interactive(_))
                    | Some(AgentEntry::Terminal(_))
                    | Some(AgentEntry::Orphaned(_))
                    | Some(AgentEntry::Group(_)) => {}
                    None => self.focus = Focus::Preview,
                }
            }
        }
    }
}
