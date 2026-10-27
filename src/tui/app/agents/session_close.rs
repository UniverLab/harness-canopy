use super::{remove_session_name, reverse_sorted_indices, selection_after_mutation, SessionTarget};
use crate::tui::agent::AgentStatus;
use crate::tui::app::types::{AgentEntry, App};
use crate::tui::terminal_history::save_history;
use std::time::Duration;

const SHADOW_SUMMARY_LINGER_SECS: u64 = 7;
const SHADOW_SUMMARY_INSTRUCTION: &str = "Session terminated by user. Before exit, call intelligence_upsert with kind='fact' and persist a concise summary including: mission outcome, key decisions, pending follow-ups, and any reusable facts/patterns.";

impl App {
    pub(super) fn remove_terminal_sessions(&mut self, indices: Vec<usize>) {
        for idx in reverse_sorted_indices(indices) {
            let _ = self.remove_session_target(SessionTarget::Terminal(idx));
        }
    }

    pub(super) fn remove_interactive_sessions(&mut self, indices: Vec<usize>) {
        for idx in reverse_sorted_indices(indices) {
            let _ = self.remove_session_target(SessionTarget::Interactive(idx));
        }
    }

    pub(super) fn close_session_target(&mut self, target: SessionTarget, exit_code: i32) -> bool {
        match target {
            SessionTarget::Interactive(idx) => self.close_interactive_session_at(idx, exit_code),
            SessionTarget::Terminal(idx) => self.close_terminal_session_at(idx),
        }
    }

    pub(super) fn remove_session_target(&mut self, target: SessionTarget) -> bool {
        match target {
            SessionTarget::Interactive(idx) => self.remove_interactive_session_entry(idx),
            SessionTarget::Terminal(idx) => self.remove_terminal_session_entry(idx),
        }
    }

    pub(super) fn delete_group_at(&mut self, idx: usize) -> bool {
        let Some(group_id) = self.split_groups.get(idx).map(|group| group.id.clone()) else {
            return false;
        };

        self.delete_group_by_id(&group_id)
    }

    pub(super) fn delete_group_by_id(&mut self, id: &str) -> bool {
        if !self.split_groups.iter().any(|group| group.id == id) {
            return false;
        }

        let _ = self.db.delete_group(id);
        self.split_groups.retain(|group| group.id != id);
        if self.active_split_id.as_deref() == Some(id) {
            self.active_split_id = None;
        }
        true
    }

    pub fn terminate_focused_session(&mut self) {
        let terminated = if self.active_split_id.is_some() {
            self.terminate_active_split_session()
        } else {
            self.terminate_selected_session()
        };
        if !terminated {
            return;
        }

        self.finish_session_mutation();
    }

    pub(super) fn kill_session_by_name(&mut self, name: &str) {
        let Some(target) = self.session_target_by_name(name) else {
            return;
        };

        let _ = self.close_session_target(target, 0);
    }

    pub(super) fn finish_session_mutation(&mut self) {
        let _ = self.refresh_agents();
        self.sync_selection_after_session_mutation();
        self.reset_focus_after_session_mutation();
    }

    pub(super) fn selected_entry_identity(&self) -> Option<String> {
        self.agents
            .get(self.selected)
            .map(|entry| entry.id(self).to_string())
    }

    pub(super) fn finish_session_mutation_preserving(&mut self, anchor: Option<&str>) {
        let _ = self.refresh_agents();
        let ids: Vec<String> = self
            .agents
            .iter()
            .map(|entry| entry.id(self).to_string())
            .collect();
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        self.selected = selection_after_mutation(&id_refs, anchor, self.selected);
        self.reset_focus_after_session_mutation();
    }

    pub(super) fn close_interactive_session_at(&mut self, idx: usize, exit_code: i32) -> bool {
        let Some(agent_id) = self
            .interactive_agents
            .get(idx)
            .map(|agent| agent.id.clone())
        else {
            return false;
        };

        let _ = self.db.finish_interactive_session(&agent_id, exit_code);
        let Some(agent) = self.interactive_agents.get(idx) else {
            return false;
        };
        // The shadow kill rides a thread that dies with the process: keep the
        // stream here too so quitting within the linger window can still
        // send it (`App::cleanup`) instead of losing the close.
        self.pending_closes.push(agent.stream.clone());
        agent.schedule_shadow_shutdown(
            SHADOW_SUMMARY_INSTRUCTION,
            Duration::from_secs(SHADOW_SUMMARY_LINGER_SECS),
        );
        self.remove_session_target(SessionTarget::Interactive(idx))
    }

    pub(super) fn close_terminal_session_at(&mut self, idx: usize) -> bool {
        let Some(agent_id) = self.terminal_agents.get(idx).map(|agent| agent.id.clone()) else {
            return false;
        };

        let _ = self.db.finish_terminal_session(&agent_id);
        let Some(agent) = self.terminal_agents.get_mut(idx) else {
            return false;
        };

        // Design decision: keep scrollback as plain text and exclude
        // full-screen programs, rather than switch to storing a replayable
        // raw ANSI byte stream.
        //
        // A byte stream would let a full-screen app (vim, htop, a nested
        // canopy) replay faithfully, but it changes what lands on disk in
        // ways this spec explicitly flags as needing its own size/safety
        // review, and it would still have to solve accumulation (the same
        // append-forever bug that corrupts the current text format).
        // Excluding full-screen output instead reuses the existing
        // plain-text, line-capped format and needs no new on-disk shape.
        //
        // vt100 already tells us when a full-screen program is active:
        // `Screen::alternate_screen()` mirrors DECSET 1049, and the crate
        // never threads the alternate grid into scrollback (it's allocated
        // with `scrollback_len: 0` — see vt100::Screen::new). So if the
        // session is still in alternate-screen mode at close time,
        // `last_lines` would only be able to return the program's current
        // on-screen frame (there's no line history to fall back to), and
        // persisting that as scrollback text is exactly the "rendered
        // frames printed back as literal text" bug. Skip the capture
        // entirely in that case and keep whatever was already persisted —
        // once the program exits back to the shell, a later close captures
        // real shell history again, since alternate-grid content never
        // reached the primary grid's scrollback in the first place.
        if !agent.in_alternate_screen() {
            let scrollback = agent.last_lines(2000);
            if let Some(hist) = self.terminal_histories.get_mut(&agent.name) {
                let lines: Vec<String> = scrollback.lines().map(|s| s.to_string()).collect();
                hist.update_scrollback(&lines);
                save_history(&self.data_dir, &agent.name, hist);
            }
        }
        agent.kill();
        self.remove_session_target(SessionTarget::Terminal(idx))
    }

    pub(super) fn remove_interactive_session_entry(&mut self, idx: usize) -> bool {
        let Some(agent_name) = remove_session_name(&mut self.interactive_agents, idx) else {
            return false;
        };

        self.dissolve_groups_for_session(&agent_name);
        true
    }

    pub(super) fn remove_terminal_session_entry(&mut self, idx: usize) -> bool {
        let Some(agent_name) = remove_session_name(&mut self.terminal_agents, idx) else {
            return false;
        };

        self.dissolve_groups_for_session(&agent_name);
        true
    }

    pub fn delete_selected(&mut self) -> anyhow::Result<()> {
        let Some(selected) = self.selected_agent() else {
            return Ok(());
        };

        match selected {
            AgentEntry::Agent(agent) => {
                use crate::application::ports::AgentRepository;
                self.db.delete_agent(&agent.id)?;
            }
            AgentEntry::Corrupt(corrupt) => {
                // Deletes by id without parsing the stored row — a corrupt
                // row must always be removable.
                use crate::application::ports::AgentRepository;
                self.db.delete_agent(&corrupt.id)?;
            }
            AgentEntry::Group(idx) => {
                if !self.delete_group_at(*idx) {
                    return Ok(());
                }
            }
            AgentEntry::Interactive(idx) => {
                if !self.close_session_target(SessionTarget::Interactive(*idx), 0) {
                    return Ok(());
                }
            }
            AgentEntry::Terminal(idx) => {
                if !self.close_session_target(SessionTarget::Terminal(*idx), 0) {
                    return Ok(());
                }
            }
            AgentEntry::Orphaned(idx) => {
                // Orphaned sessions are DB-only; just drop the entry.
                self.orphaned_sessions.remove(*idx);
                self.finish_session_mutation();
            }
        }

        self.finish_session_mutation();
        Ok(())
    }

    pub(crate) fn dissolve_groups_for_session(&mut self, session_name: &str) {
        let ids_to_dissolve: Vec<String> = self
            .split_groups
            .iter()
            .filter(|group| group.session_a == session_name || group.session_b == session_name)
            .map(|group| group.id.clone())
            .collect();

        for id in ids_to_dissolve {
            let _ = self.delete_group_by_id(&id);
        }
    }

    pub(crate) fn selected_session_is_exited(&self) -> bool {
        match self.selected_session_target() {
            Some(SessionTarget::Interactive(idx)) => self
                .interactive_agents
                .get(idx)
                .is_some_and(|agent| matches!(agent.status, AgentStatus::Exited(_))),
            Some(SessionTarget::Terminal(idx)) => self
                .terminal_agents
                .get(idx)
                .is_some_and(|agent| matches!(agent.status, AgentStatus::Exited(_))),
            None => false,
        }
    }

    pub(crate) fn dismiss_selected_exited_session(&mut self) {
        // The session already exited (DB was finalized in handle_*_exit); just drop
        // it from the list and let the selection clamp to a neighbour.
        let Some(target) = self.selected_session_target() else {
            return;
        };
        if self.remove_session_target(target) {
            self.finish_session_mutation();
        }
    }

    /// Revive a selected orphaned session by re-launching its CLI with stored
    /// args in its stored workdir.
    pub(crate) fn revive_selected_orphaned_session(&mut self) {
        let Some(AgentEntry::Orphaned(idx)) = self.selected_agent() else {
            return;
        };
        let idx = *idx;
        let Some(session) = self.orphaned_sessions.get(idx).cloned() else {
            return;
        };

        let home = dirs::home_dir().unwrap_or_default();
        let canopy_config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
        let (cols, rows) = Self::session_panel_size();
        let current_boot_id = crate::system::boot_id();

        // Remove from orphaned list first.
        self.orphaned_sessions.remove(idx);
        self.resume_interactive_session(
            &session,
            &canopy_config,
            cols,
            rows,
            current_boot_id.as_deref(),
        );
        self.finish_session_mutation();
    }

    /// Dismiss (remove) a selected orphaned session without reviving it.
    pub(crate) fn dismiss_selected_orphaned_session(&mut self) {
        let Some(AgentEntry::Orphaned(idx)) = self.selected_agent() else {
            return;
        };
        let idx = *idx;
        self.orphaned_sessions.remove(idx);
        self.finish_session_mutation();
    }
}
