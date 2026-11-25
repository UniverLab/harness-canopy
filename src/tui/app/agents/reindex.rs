use crate::tui::app::types::{AgentEntry, App, Focus};

impl App {
    /// Rebuild the sidebar's session-derived entries from the live in-memory
    /// session lists so `agents` and `selected` never hold a stale index.
    /// Background-agent and corrupt rows are kept as the prefix; this is
    /// DB-free and safe to call from the hub dispatch tick.
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
                    | Some(AgentEntry::Orphaned(_)) => {}
                    None => self.focus = Focus::Preview,
                }
            }
        }
    }
}
