//! Prompt builder session persistence (split from `dialog::prompt`, pure move).
use std::collections::{HashMap, HashSet};

use super::{PromptTab, SectionPickerMode, SendChoice, SimplePromptDialog};

/// Snapshot of `SimplePromptDialog` state used to persist the prompt builder
/// per agent/session across openings within the same canopy TUI session.
#[derive(Clone)]
pub struct PromptBuilderSession {
    pub sections: HashMap<String, String>,
    pub enabled_sections: Vec<String>,
    pub section_counters: HashMap<String, usize>,
    pub section_cursors: HashMap<String, usize>,
    pub section_scrolls: HashMap<String, usize>,
    pub collapsed_pastes: HashMap<String, String>,
    pub locked_sections: HashSet<String>,
    pub send_choice: SendChoice,
    pub send_at: Option<chrono::NaiveDateTime>,
    /// Which tab was active when the builder was hidden. The Raw buffer itself
    /// rides along inside `sections` under `RAW_SECTION_ID`.
    pub active_tab: PromptTab,
}

impl PromptBuilderSession {
    pub fn from_dialog(dialog: &SimplePromptDialog) -> Self {
        Self {
            sections: dialog.sections.clone(),
            enabled_sections: dialog.enabled_sections.clone(),
            section_counters: dialog.section_counters.clone(),
            section_cursors: dialog.section_cursors.clone(),
            section_scrolls: dialog.section_scrolls.clone(),
            collapsed_pastes: dialog.collapsed_pastes.clone(),
            locked_sections: dialog.locked_sections.clone(),
            send_choice: dialog.send_choice,
            send_at: dialog.send_at,
            active_tab: dialog.active_tab,
        }
    }

    pub fn restore_into(&self, dialog: &mut SimplePromptDialog) {
        dialog.sections = self.sections.clone();
        dialog.enabled_sections = self.enabled_sections.clone();
        dialog.section_counters = self.section_counters.clone();
        dialog.section_cursors = self.section_cursors.clone();
        dialog.section_scrolls = self.section_scrolls.clone();
        dialog.collapsed_pastes = self.collapsed_pastes.clone();
        dialog.locked_sections = self.locked_sections.clone();
        dialog.send_choice = self.send_choice;
        dialog.send_at = self.send_at;
        dialog.active_tab = self.active_tab;
        // A reopened builder always starts in the first section, regardless of
        // where focus sat when the session was captured — the send control is
        // the last stop of the cycle, never the entry point. On the Raw tab
        // that first stop is instead the raw buffer (focus index 1).
        match self.active_tab {
            PromptTab::Raw => dialog.focused_section = 1,
            PromptTab::Normal => dialog.focus_first_section(),
        }
        // Reset transient UI state (not persisted across openings)
        dialog.picker_mode = SectionPickerMode::None;
        dialog.at_picker = None;
        dialog.system_content = None; // re-evaluated on each open
        dialog.protocol_included = false; // re-evaluated on each open
        dialog.send_edit = None;
        dialog.send_error = None;
        dialog.raw_preview = None; // recomputed when the Raw tab is shown
        dialog.scheduled_list_selected = None; // list focus never persists
        dialog.editing_scheduled_id = None; // a reopened builder isn't mid-edit
    }
}

/// JSON-serializable snapshot of the builder's structured fields, persisted
/// per project workdir as `last_prompts.builder_state` (U8) so Ctrl+L can
/// rebuild the builder as it was rather than pasting a flattened blob.
///
/// Deliberately excludes `send_at`: recalling a prompt should not silently
/// re-arm a delivery schedule from a previous session. `picker_mode`,
/// `at_picker`, `system_content`, and `protocol_included` are transient
/// UI/idempotency state that `PromptBuilderSession::restore_into` also
/// never persists.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PersistedBuilderState {
    pub sections: HashMap<String, String>,
    pub enabled_sections: Vec<String>,
    pub focused_section: usize,
    pub section_counters: HashMap<String, usize>,
    pub section_cursors: HashMap<String, usize>,
    pub section_scrolls: HashMap<String, usize>,
    pub collapsed_pastes: HashMap<String, String>,
    pub locked_sections: HashSet<String>,
}

impl PersistedBuilderState {
    /// Canonical minimal structured state for a single instruction prompt —
    /// the same shape [`SimplePromptDialog::new`] produces, with
    /// `instruction_1` holding `prompt`. Used by graph interactive hooks so a
    /// hook-enqueued scheduled send carries the structured representation an
    /// equivalent promptbuilder message would have, rather than an ad-hoc
    /// JSON blob the builder cannot restore. The raw prompt itself is
    /// unchanged; this is only the reopen/edit representation.
    pub fn for_instruction_prompt(prompt: &str) -> Self {
        let mut sections = HashMap::new();
        sections.insert("instruction_1".to_string(), prompt.to_string());
        let mut section_counters = HashMap::new();
        section_counters.insert("instruction".to_string(), 2usize);
        section_counters.insert("context".to_string(), 2usize);
        let mut section_cursors = HashMap::new();
        section_cursors.insert("instruction_1".to_string(), 0usize);
        let mut section_scrolls = HashMap::new();
        section_scrolls.insert("instruction_1".to_string(), 0usize);
        Self {
            sections,
            enabled_sections: vec!["instruction_1".to_string()],
            focused_section: 1,
            section_counters,
            section_cursors,
            section_scrolls,
            collapsed_pastes: HashMap::new(),
            locked_sections: HashSet::new(),
        }
    }

    pub fn from_dialog(dialog: &SimplePromptDialog) -> Self {
        Self {
            sections: dialog.sections.clone(),
            enabled_sections: dialog.enabled_sections.clone(),
            focused_section: dialog.focused_section,
            section_counters: dialog.section_counters.clone(),
            section_cursors: dialog.section_cursors.clone(),
            section_scrolls: dialog.section_scrolls.clone(),
            collapsed_pastes: dialog.collapsed_pastes.clone(),
            locked_sections: dialog.locked_sections.clone(),
        }
    }

    pub fn restore_into(&self, dialog: &mut SimplePromptDialog) {
        dialog.sections = self.sections.clone();
        dialog.enabled_sections = self.enabled_sections.clone();
        dialog.section_counters = self.section_counters.clone();
        dialog.section_cursors = self.section_cursors.clone();
        dialog.section_scrolls = self.section_scrolls.clone();
        dialog.collapsed_pastes = self.collapsed_pastes.clone();
        dialog.locked_sections = self.locked_sections.clone();
        // A recalled prompt opens focused on the first section, ready to type —
        // never on the send control (focus 0), whatever was persisted.
        dialog.focus_first_section();
        dialog.picker_mode = SectionPickerMode::None;
        dialog.at_picker = None;
    }
}
