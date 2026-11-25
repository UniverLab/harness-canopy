//! App context transfer methods moved from the composition root.

use super::*;

impl App {
    // ── Context Transfer ────────────────────────────────────────

    /// Open the context transfer modal for the currently focused interactive or terminal agent.
    pub fn open_context_transfer_modal(&mut self) {
        let source = self.selected_context_transfer_source();
        self.open_context_transfer_from_source(source);
    }

    /// Close the modal and return focus to the agent.
    pub fn close_context_transfer_modal(&mut self) {
        self.context_transfer_modal = None;
        self.focus = Focus::Agent;
    }

    /// Advance the modal from Preview to AgentPicker.
    pub fn context_transfer_to_picker(&mut self) {
        let Some(modal) = &mut self.context_transfer_modal else {
            return;
        };
        if modal.step != ContextTransferStep::Preview {
            return;
        }

        modal.step = ContextTransferStep::AgentPicker;
        modal.picker_selected = 0;
    }

    pub(crate) fn interactive_picker_destination(&self, dest_entry_idx: usize) -> Option<usize> {
        self.picker_interactive_entries()
            .get(dest_entry_idx)
            .copied()
            .filter(|idx| *idx < self.interactive_agents.len())
    }

    pub(crate) fn focus_interactive_agent(&mut self, dest_ia_idx: usize) {
        if let Some(entry_pos) = self.find_agent_entry_position(dest_ia_idx) {
            self.selected = entry_pos;
        }
        self.focus = Focus::Agent;
    }

    pub(crate) fn find_agent_entry_position(&self, dest_ia_idx: usize) -> Option<usize> {
        self.agents
            .iter()
            .position(|entry| matches!(entry, AgentEntry::Interactive(idx) if *idx == dest_ia_idx))
    }

    pub(crate) fn open_context_prompt_dialog(
        &mut self,
        context_payload: String,
        rag_query: Option<String>,
    ) {
        let mut initial_content = HashMap::from([("context".to_string(), context_payload)]);
        if let Some(query) = rag_query.filter(|query| !query.trim().is_empty()) {
            initial_content.insert("rag".to_string(), format!("global: {query}"));
        }
        self.open_simple_prompt_dialog(Some(initial_content));
    }

    pub(crate) fn context_transfer_source_for_entry(
        &self,
        entry: &AgentEntry,
    ) -> Option<ContextTransferSource> {
        match entry {
            AgentEntry::Interactive(idx) => self
                .interactive_agents
                .get(*idx)
                .map(|_| ContextTransferSource::Interactive(*idx)),
            AgentEntry::Terminal(idx) => self
                .terminal_agents
                .get(*idx)
                .map(|_| ContextTransferSource::Terminal(*idx)),
            AgentEntry::Agent(_) | AgentEntry::Corrupt(_) | AgentEntry::Orphaned(_) => None,
        }
    }

    pub(crate) fn context_transfer_source_for_kind(
        &self,
        kind: ContextSourceKind,
        idx: usize,
    ) -> Option<ContextTransferSource> {
        match kind {
            ContextSourceKind::Interactive => self
                .interactive_agents
                .get(idx)
                .map(|_| ContextTransferSource::Interactive(idx)),
            ContextSourceKind::Terminal => self
                .terminal_agents
                .get(idx)
                .map(|_| ContextTransferSource::Terminal(idx)),
        }
    }

    pub(crate) fn context_transfer_agent(
        &self,
        source: ContextTransferSource,
    ) -> Option<&InteractiveAgent> {
        match source {
            ContextTransferSource::Interactive(idx) => self.interactive_agents.get(idx),
            ContextTransferSource::Terminal(idx) => self.terminal_agents.get(idx),
        }
    }

    pub(crate) fn context_transfer_source_kind(source: ContextTransferSource) -> ContextSourceKind {
        match source {
            ContextTransferSource::Interactive(_) => ContextSourceKind::Interactive,
            ContextTransferSource::Terminal(_) => ContextSourceKind::Terminal,
        }
    }

    /// Execute the context transfer to the selected destination agent.
    ///
    /// 1. Builds the payload.
    /// 2. Switches focus to destination.
    /// 3. Opens Prompt Template dialog with payload pre-filled in the "context" section.
    pub fn execute_context_transfer(&mut self, dest_entry_idx: usize) {
        let Some(modal) = self.context_transfer_modal.take() else {
            return;
        };
        let Some(dest_ia_idx) = self.interactive_picker_destination(dest_entry_idx) else {
            return;
        };
        let Some(payload) = self.build_context_transfer_payload(&modal) else {
            return;
        };

        self.focus_interactive_agent(dest_ia_idx);
        self.open_context_prompt_dialog(payload, None);
    }

    pub(crate) fn refresh_context_transfer_preview(&mut self) {
        let Some((source, parts)) = self
            .context_transfer_modal
            .as_ref()
            .and_then(|modal| self.modal_source(modal).map(|source| (source, modal.parts)))
        else {
            return;
        };

        let Some(preview) = self.build_context_transfer_payload_from_source(source, parts) else {
            return;
        };

        if let Some(modal) = self.context_transfer_modal.as_mut() {
            modal.payload_preview = preview;
        }
    }

    pub(crate) fn selected_context_transfer_source(&self) -> Option<ContextTransferSource> {
        self.selected_agent()
            .and_then(|entry| self.context_transfer_source_for_entry(entry))
    }

    pub(crate) fn open_context_transfer_from_source(
        &mut self,
        source: Option<ContextTransferSource>,
    ) {
        let Some(source) = source else {
            return;
        };
        let Some(mut modal) = self.modal_for_context_transfer_source(source) else {
            return;
        };

        if let Some(preview) = self.build_context_transfer_payload(&modal) {
            modal.payload_preview = preview;
        }

        modal.prev_focus = Some(self.focus);
        self.context_transfer_modal = Some(modal);
        self.focus = Focus::ContextTransfer;
    }

    pub(crate) fn modal_for_context_transfer_source(
        &self,
        source: ContextTransferSource,
    ) -> Option<ContextTransferModal> {
        match source {
            ContextTransferSource::Interactive(idx) => self.build_interactive_transfer_modal(idx),
            ContextTransferSource::Terminal(idx) => self.build_terminal_transfer_modal(idx),
        }
    }

    pub(crate) fn build_interactive_transfer_modal(
        &self,
        idx: usize,
    ) -> Option<ContextTransferModal> {
        self.context_transfer_agent(ContextTransferSource::Interactive(idx))?;
        Some(ContextTransferModal::new(idx))
    }

    pub(crate) fn build_terminal_transfer_modal(&self, idx: usize) -> Option<ContextTransferModal> {
        self.context_transfer_agent(ContextTransferSource::Terminal(idx))?;
        Some(ContextTransferModal::new_terminal(idx))
    }

    pub(crate) fn modal_source(
        &self,
        modal: &ContextTransferModal,
    ) -> Option<ContextTransferSource> {
        self.context_transfer_source_for_kind(modal.source_kind(), modal.source_agent_idx)
    }

    pub(crate) fn build_context_transfer_payload(
        &self,
        modal: &ContextTransferModal,
    ) -> Option<String> {
        self.build_context_transfer_payload_from_source(self.modal_source(modal)?, modal.parts)
    }

    pub(crate) fn build_context_transfer_payload_from_source(
        &self,
        source: ContextTransferSource,
        parts: ContextTransferParts,
    ) -> Option<String> {
        let agent = self.context_transfer_agent(source)?;
        Some(build_context_payload_for(
            agent,
            Self::context_transfer_source_kind(source),
            parts,
        ))
    }

    /// Collect interactive agent indices for use in the picker list.
    pub fn picker_interactive_entries(&self) -> Vec<usize> {
        (0..self.interactive_agents.len()).collect()
    }

    pub fn open_rag_transfer_modal(&mut self) {
        let Some(chunk) = self.selected_playground_chunk() else {
            return;
        };

        let query = self.playground_query.trim().to_string();
        let context_payload = format!(
            "kind: rag_chunk\nquery: {}\npath: {}\ndistance: {}\ncontent:\n{}",
            query,
            chunk.file_path,
            chunk
                .distance
                .map_or("—".to_string(), |d| format!("{d:.4}")),
            chunk.content
        );

        self.rag_transfer_modal = Some(RagTransferModal {
            picker_selected: 0,
            query,
            context_payload,
            prev_focus: Some(self.focus),
        });
        self.focus = Focus::RagTransfer;
    }

    pub fn close_rag_transfer_modal(&mut self) {
        self.rag_transfer_modal = None;
        self.focus = Focus::Preview;
    }

    pub fn execute_rag_transfer(&mut self, dest_entry_idx: usize) {
        let Some(modal) = self.rag_transfer_modal.take() else {
            return;
        };
        let Some(dest_ia_idx) = self.interactive_picker_destination(dest_entry_idx) else {
            return;
        };

        self.focus_interactive_agent(dest_ia_idx);
        self.open_context_prompt_dialog(modal.context_payload, Some(modal.query));
    }
}
