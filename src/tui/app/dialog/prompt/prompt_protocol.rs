//! The session-start Canopy protocol block inside the Prompt Builder:
//! when a send carries it, and how the manual `Canopy protocol` section
//! resends it on purpose (spec FR4/FR5).
//!
//! The text always comes from [`crate::domain::sandbox::canopy_protocol_block`]
//! — never copied here — so this path can never drift from the sandbox's
//! instruction file.

use super::*;

impl SimplePromptDialog {
    /// Whether this send will carry the protocol block: the automatic
    /// first-turn flag (`protocol_included`, decided when the dialog
    /// opened) OR the user-added `Canopy protocol` section. A non-empty
    /// Raw tab sends the buffer verbatim and carries neither, so it never
    /// counts as "protocol included".
    pub fn sends_protocol(&self) -> bool {
        self.protocol_in_prompt() && !(self.active_tab == PromptTab::Raw && !self.raw_is_empty())
    }

    /// Whether the composed system block will include the protocol:
    /// the automatic flag or an enabled `Canopy protocol` section.
    pub fn protocol_in_prompt(&self) -> bool {
        self.protocol_included || self.has_canopy_protocol_section()
    }

    /// Whether the `Canopy protocol` section is enabled (any instance).
    pub fn has_canopy_protocol_section(&self) -> bool {
        self.enabled_sections
            .iter()
            .any(|id| Self::section_matches_prefix(id, "canopy_protocol"))
    }

    /// Add the `Canopy protocol` section: locked (read-only) and pre-filled
    /// with the canonical block. Removal is unchanged — the remove-section
    /// picker ignores locks, which only stop edits, so it can be taken out
    /// like any other section.
    pub fn add_canopy_protocol_section(&mut self) {
        let block = crate::domain::sandbox::canopy_protocol_block().to_string();
        let id = self.add_section_with_content("canopy_protocol", block);
        self.lock_section(&id);
    }

    /// The assembled system-block content: the protocol once at the very
    /// top (when it rides along), then the per-turn context. `system_content`
    /// itself never carries the block, so an automatic send and a manual
    /// `Canopy protocol` section can never duplicate it.
    pub fn system_text(&self) -> String {
        let turn_context = self.system_content.clone().unwrap_or_default();
        if !self.protocol_in_prompt() {
            return turn_context;
        }
        let block = crate::domain::sandbox::canopy_protocol_block();
        if turn_context.is_empty() {
            return block.to_string();
        }
        format!("{block}\n\n{turn_context}")
    }
}
