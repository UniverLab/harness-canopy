use anyhow::Result;
use chrono::Timelike;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::collections::{HashMap, HashSet};

use super::at_picker::AtPicker;
use crate::db::Database;
use crate::domain::project::Project;
use crate::tui::app::types::Focus;
use std::path::Path;

// `prompt_edit` and `prompt_schedule` hold only `impl SimplePromptDialog`
// blocks, so there is nothing to re-export here: inherent methods are
// reachable through the type from anywhere in the crate.
mod prompt_build;
use prompt_build::*;
mod prompt_edit;
mod prompt_schedule;
// `prompt_protocol` holds only an `impl SimplePromptDialog` block, so
// there is nothing to re-export (same as `prompt_edit`/`prompt_schedule`).
mod prompt_protocol;

/// Picker state for adding/removing sections
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SectionPickerMode {
    #[default]
    None,
    AddSection {
        selected: usize,
    },
    RemoveSection {
        selected: usize,
        /// Scroll offset into `enabled_sections`' removable subset — unlike
        /// `AddSection`'s fixed, short menu this list grows with however
        /// many sections the user has enabled, so it can exceed the
        /// viewport in practice.
        scroll: usize,
    },
    AddCustom {
        input: String,
    },
    /// Skills picker for the Tools section — entries are `(label, raw_name, prefix)`
    SkillsPicker {
        selected: usize,
        scroll: usize,
        /// `(display_label, raw_name, prefix)` — `prefix` is "skill" or "global"
        entries: Vec<(String, String, String)>,
        /// `None` → create a new tools section on confirm; `Some(id)` → replace content of that section
        replace_id: Option<String>,
    },
    /// `selected`/`scroll` index into the FILTERED list (mirroring
    /// `PresetPicker`), so typing narrows `entries` in place without a
    /// re-query while the highlighted row and scroll window stay in sync.
    ProjectPicker {
        selected: usize,
        entries: Vec<ProjectPickerEntry>,
        filter: String,
        scroll: usize,
    },
    /// Prompt-preset picker (P2): entries are read fresh from
    /// `~/.canopy/prompts/*.md` (P1) each time the picker opens — `(name,
    /// preview, full content)`. `filter` is the user's typed substring
    /// filter; `selected` indexes into the FILTERED list, mirroring
    /// `NewAgentDialog`'s CLI picker (`cli_picker_idx`).
    PresetPicker {
        selected: usize,
        entries: Vec<(String, String, String)>,
        filter: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub hash: String,
    pub name: String,
    pub path: String,
}

/// The send control's selector value (U11): lateral arrows toggle between
/// sending immediately and scheduling a date-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SendChoice {
    #[default]
    Now,
    Date,
}

/// Which top-of-dialog tab is active. `Normal` is the section-based form;
/// `Raw` is a single free-text field that is sent as-is (or, when empty,
/// previews the composed Normal-form prompt). Pilot of clickable "windows".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum PromptTab {
    #[default]
    Normal,
    Raw,
}

/// Section id backing the Raw tab's free-text buffer. It deliberately lives in
/// `sections`/`section_cursors`/`section_scrolls` (so all the existing editing
/// machinery applies) but is NEVER added to `enabled_sections`, so it stays
/// out of focus navigation, `is_empty`, and the composed-prompt build.
pub const RAW_SECTION_ID: &str = "__raw__";

/// Display label (with padding) of the Normal tab in the tab bar.
pub const TAB_NORMAL_LABEL: &str = " Normal ";
/// Display label (with padding) of the Raw tab in the tab bar.
pub const TAB_RAW_LABEL: &str = " Raw ";

/// Inline date-time picker state for the send control (U11). Opened with
/// Enter on `send: date`, preseeded with the current local time. An alias
/// for the shared picker ([`crate::tui::app::dialog::datetime_picker::DateTimeEdit`])
/// also used by the graph autorun dialog (C18), so the two never diverge into
/// separate widgets.
pub type SendAtEdit = crate::tui::app::dialog::datetime_picker::DateTimeEdit;

use crate::tui::app::dialog::datetime_picker::{add_months, field_digit_width, with_field};
// `days_in_month` isn't called directly outside `datetime_picker` (only
// through `with_field`) — this crate's own tests below are the sole direct
// caller, so the import is test-only.
#[cfg(test)]
use crate::tui::app::dialog::datetime_picker::days_in_month;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RagScope<'a> {
    Global,
    Project(&'a str),
}

/// New simplified prompt template dialog with dynamic sections
/// Now supports multiple instances of the same section type
pub struct SimplePromptDialog {
    /// Map of unique section IDs to their content
    pub sections: HashMap<String, String>,
    /// Ordered list of section IDs currently enabled
    pub enabled_sections: Vec<String>,
    /// Which section field is currently focused
    pub focused_section: usize,
    /// Previous focus before opening the dialog
    pub prev_focus: Option<Focus>,
    /// State for the section picker modal
    pub picker_mode: SectionPickerMode,
    /// Counter for generating unique IDs per section type
    pub section_counters: HashMap<String, usize>,
    /// Per-section cursor positions (char index)
    pub section_cursors: HashMap<String, usize>,
    /// Per-section scroll offsets (visual line)
    pub section_scrolls: HashMap<String, usize>,
    /// Active `@`-file picker (inline dropdown), if open.
    pub at_picker: Option<AtPicker>,
    /// Collapsed paste content: placeholder text is stored in `sections`,
    /// the real pasted content lives here and is used for building the prompt.
    pub collapsed_pastes: HashMap<String, String>,
    /// Section IDs that are read-only (auto-filled, cannot be edited).
    pub locked_sections: HashSet<String>,
    /// Invisible per-turn system block (workspace/intents context) rendered
    /// at the top of the final prompt. Holds no protocol text: `system_text()`
    /// prepends the one-time protocol ahead of it when it rides along. None =
    /// omit.
    pub system_content: Option<String>,
    /// Whether this send should carry the one-time session-start protocol
    /// block. `system_text()` prepends the block when this is true or the
    /// manual `Canopy protocol` section is enabled; read by `submit_prompt`
    /// to decide whether to mark the session's protocol as delivered. Decided
    /// when the dialog opens (see `App::open_simple_prompt_dialog`); never
    /// persisted across openings.
    pub protocol_included: bool,
    /// Send-timing selector (U11): `now` sends immediately, `date`
    /// schedules the delivery at `send_at`.
    pub send_choice: SendChoice,
    /// Confirmed scheduled delivery, local wall-clock (U11). Only honored
    /// when `send_choice` is [`SendChoice::Date`].
    pub send_at: Option<chrono::NaiveDateTime>,
    /// Inline date-time picker state while the user is editing (U11).
    pub send_edit: Option<SendAtEdit>,
    /// Inline validation hint for the send control (e.g. past time picked).
    pub send_error: Option<String>,
    /// A last-prompt recall (Ctrl+L) awaiting the standard confirm pattern
    /// because the builder currently has non-empty content. `None` once
    /// confirmed/canceled. Not persisted across dialog openings.
    pub pending_recall: Option<crate::db::last_prompts::LastPrompt>,
    /// Active tab at the top of the dialog (Normal form vs Raw free-text).
    pub active_tab: PromptTab,
    /// Cached composed-prompt string shown as the read-only preview when the
    /// Raw tab is active and its buffer is empty. Recomputed on entering the
    /// Raw tab (the Normal form can't change while Raw is shown). Transient —
    /// never persisted.
    pub raw_preview: Option<String>,
    /// Vertical scroll offset (in preview lines) for the read-only raw
    /// preview. Reset whenever the preview is recomputed. Transient.
    pub raw_preview_scroll: usize,
    /// Wheel-driven scroll offset for the editable raw buffer (view-only,
    /// separate from the cursor). `None` means cursor-follow is in effect;
    /// `Some(n)` means the user scrolled with the wheel and the view is
    /// pinned at line `n` until the next cursor movement clears it.
    pub raw_edit_scroll: Option<usize>,
    /// Selected row in the pending-scheduled-sends list panel (B33). `Some(i)`
    /// means the list region has keyboard focus and row `i` is highlighted;
    /// `None` means the list (if shown) is idle and keys go to the normal
    /// fields. Transient — recomputed against the live DB list on each frame.
    pub scheduled_list_selected: Option<usize>,
    /// When set, a re-confirmed schedule UPDATES this existing scheduled-send
    /// row in place (delete + re-insert) instead of creating a duplicate — the
    /// select-to-edit flow (B33). Transient; cleared once the builder closes.
    pub editing_scheduled_id: Option<String>,
}

impl SimplePromptDialog {
    pub fn new() -> Self {
        let mut counters = HashMap::new();
        counters.insert("instruction".to_string(), 2usize);
        counters.insert("context".to_string(), 2usize);
        let mut cursors = HashMap::new();
        cursors.insert("instruction_1".to_string(), 0usize);
        let mut scrolls = HashMap::new();
        scrolls.insert("instruction_1".to_string(), 0usize);
        let mut sections = HashMap::new();
        sections.insert("instruction_1".to_string(), String::new());
        Self {
            sections,
            enabled_sections: vec!["instruction_1".to_string()],
            // Focus starts on the first section — the send control (virtual
            // index 0) sits at the bottom and is reached by wrapping (U11).
            focused_section: 1,
            prev_focus: None,
            picker_mode: SectionPickerMode::None,
            section_counters: counters,
            section_cursors: cursors,
            section_scrolls: scrolls,
            at_picker: None,
            collapsed_pastes: HashMap::new(),
            locked_sections: HashSet::new(),
            system_content: None,
            protocol_included: false,
            send_choice: SendChoice::Now,
            send_at: None,
            send_edit: None,
            send_error: None,
            pending_recall: None,
            active_tab: PromptTab::Normal,
            raw_preview: None,
            raw_preview_scroll: 0,
            raw_edit_scroll: None,
            scheduled_list_selected: None,
            editing_scheduled_id: None,
        }
    }

    /// True when every enabled section is blank — the state Ctrl+L's confirm
    /// pattern treats as "safe to overwrite without asking".
    pub fn is_empty(&self) -> bool {
        self.enabled_sections.iter().all(|section_id| {
            self.section_content_for_build(section_id)
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        })
    }

    /// Replace all builder content with a single instruction section holding
    /// `text` verbatim. Used to recall a prompt whose structured builder
    /// state wasn't captured — a scheduled send recovered after its target
    /// session died only has the flattened prompt string (see
    /// `Database::insert_failed_scheduled_send`), so a faithful
    /// section-by-section restore isn't possible.
    pub fn load_flat_text(&mut self, text: &str) {
        self.sections.clear();
        self.enabled_sections.clear();
        self.section_cursors.clear();
        self.section_scrolls.clear();
        self.collapsed_pastes.clear();
        self.locked_sections.clear();
        self.section_counters.clear();
        self.section_counters.insert("instruction".to_string(), 2);

        let cursor = text.chars().count();
        self.sections
            .insert("instruction_1".to_string(), text.to_string());
        self.enabled_sections.push("instruction_1".to_string());
        self.section_cursors
            .insert("instruction_1".to_string(), cursor);
        self.section_scrolls.insert("instruction_1".to_string(), 0);
        self.focused_section = 1; // 0 is the send_at virtual field
        self.picker_mode = SectionPickerMode::None;
        self.at_picker = None;
    }

    /// Get cursor position for a section
    pub fn cursor(&self, section: &str) -> usize {
        self.section_cursors.get(section).copied().unwrap_or(0)
    }

    /// Get scroll offset for a section
    pub fn scroll(&self, section: &str) -> usize {
        self.section_scrolls.get(section).copied().unwrap_or(0)
    }

    /// Drive the `@`-picker's debounced search. Call once per UI tick so a
    /// pending search runs after typing pauses.
    pub fn tick_at_picker(&mut self) {
        if let Some(picker) = self.at_picker.as_mut() {
            picker.tick_search();
        }
    }

    /// Returns true if the section is locked (read-only).
    pub fn is_locked(&self, section_id: &str) -> bool {
        self.locked_sections.contains(section_id)
    }

    /// Mark a section as locked (read-only).
    pub fn lock_section(&mut self, section_id: &str) {
        self.locked_sections.insert(section_id.to_string());
    }

    /// Generate unique ID for a section instance (always uses `name_N` format, N starting at 1).
    fn generate_section_id(&mut self, section_name: &str) -> String {
        let counter = self
            .section_counters
            .entry(section_name.to_string())
            .or_insert(1);
        let id = format!("{}_{}", section_name, counter);
        *counter += 1;
        id
    }

    fn section_type(section_id: &str) -> &str {
        Self::get_available_sections()
            .into_iter()
            .map(|(name, _)| name)
            .find(|name| section_id == *name || section_id.starts_with(&format!("{name}_")))
            .unwrap_or(section_id)
    }

    fn section_matches_prefix(section_id: &str, prefix: &str) -> bool {
        section_id == prefix || section_id.starts_with(&format!("{prefix}_"))
    }

    fn instruction_count(&self) -> usize {
        self.enabled_sections
            .iter()
            .filter(|section_id| Self::section_matches_prefix(section_id, "instruction"))
            .count()
    }

    fn insert_section(&mut self, section_name: &str, content: String) -> String {
        let unique_id = self.generate_section_id(section_name);
        let cursor_pos = content.chars().count();
        self.enabled_sections.push(unique_id.clone());
        self.sections.insert(unique_id.clone(), content);
        self.section_cursors.insert(unique_id.clone(), cursor_pos);
        self.section_scrolls.insert(unique_id.clone(), 0);
        self.collapsed_pastes.remove(&unique_id);
        // Focus the newly added section: enabled_sections index (len-1) maps to
        // focus index (len) because focus 0 is the virtual send control.
        self.focused_section = self.enabled_sections.len();
        unique_id
    }

    /// Add a section instance (can be same type multiple times)
    pub fn add_section(&mut self, section_name: &str) {
        self.insert_section(section_name, String::new());
    }

    /// Add a section with pre-existing content (used for context transfer and initial content).
    /// Returns the generated section ID.
    pub fn add_section_with_content(&mut self, section_name: &str, content: String) -> String {
        self.insert_section(section_name, content)
    }

    /// Remove a specific section instance.
    /// The last remaining instruction section cannot be removed.
    pub fn remove_section(&mut self, section_id: &str) {
        if Self::section_matches_prefix(section_id, "instruction") && self.instruction_count() <= 1
        {
            return;
        }

        self.enabled_sections.retain(|s| s != section_id);
        self.sections.remove(section_id);
        self.section_cursors.remove(section_id);
        self.section_scrolls.remove(section_id);
        self.collapsed_pastes.remove(section_id);
        if self.focused_section > 0 {
            self.focused_section = self.focused_section.saturating_sub(1);
        }
    }

    /// Get available section types (these can always be added again).
    /// RAG Search is only included when an embeddings model is configured.
    pub fn get_available_sections() -> Vec<(&'static str, &'static str)> {
        let rag_enabled = dirs::home_dir()
            .map(|h| {
                let config = crate::domain::canopy_config::CanopyConfig::load(&h.join(".canopy"));
                !config.embeddings_model.trim().is_empty()
            })
            .unwrap_or(false);

        let mut sections = vec![
            ("instruction", "Instruction"),
            ("goal", "Goal"),
            ("context", "Context"),
            ("project_context", "Project Context"),
            ("resources", "Resources"),
        ];
        if rag_enabled {
            sections.push(("rag", "RAG Search"));
        }
        sections.extend([("constraints", "Constraints"), ("tools", "Tools")]);
        // The manual resend path: a locked, read-only section holding the
        // canonical protocol block (see `prompt_protocol`).
        sections.push(("canopy_protocol", "Canopy protocol"));
        sections.push(("preset", "Preset"));
        sections
    }

    /// Return true if this section ID represents the read-only "tools" section.
    pub fn is_tools_section(section_id: &str) -> bool {
        section_id == "tools" || section_id.starts_with("tools_")
    }

    /// Collect all available skills for the skills picker.
    /// Returns `Vec<(display_label, raw_name, prefix)>`.
    pub fn collect_skills_for_picker(
        workdir: &std::path::Path,
        store: Option<&crate::dynamic_skills::SkillStore>,
    ) -> Vec<(String, String, String)> {
        let mut entries: Vec<(String, String, String)> = Vec::new();
        let project = workdir.join(".agents").join("skills");
        add_skills_from_dir(&project, "skill", &mut entries);
        if let Some(global) = dirs::home_dir().map(|h| h.join(".agents").join("skills")) {
            if global != project {
                add_skills_from_dir(&global, "global", &mut entries);
            }
        }
        if let Some(s) = store {
            for skill in s.list_local() {
                let label = format!("canopy:{}", skill.name);
                entries.push((label, skill.name.clone(), "canopy".to_string()));
            }
        }
        entries
    }

    /// Collect prompt presets for the Preset picker (P2): every `*.md` file
    /// under `dir` (typically `~/.canopy/prompts/`, see
    /// `domain::prompts::prompts_dir`), sorted by name. Returns `(name,
    /// preview, full content)` where `preview` is the file's first
    /// non-empty line. Read fresh at picker-open time — no caching — so an
    /// external edit shows up immediately (spec P2). A missing directory
    /// yields an empty list rather than an error; an unreadable file is
    /// skipped rather than aborting the whole listing.
    pub fn collect_presets_for_picker(dir: &std::path::Path) -> Vec<(String, String, String)> {
        crate::domain::prompt_catalog::collect_preset_entries(dir)
            .into_iter()
            .map(|entry| (entry.name, entry.preview, entry.content))
            .collect()
    }

    /// Indices into `entries` whose name matches `filter` as a
    /// case-insensitive substring — the same "fuzzy" filter convention
    /// `NewAgentDialog::filtered_cli_indices` uses for the CLI picker.
    pub fn filtered_preset_indices(
        entries: &[(String, String, String)],
        filter: &str,
    ) -> Vec<usize> {
        let query = filter.trim().to_lowercase();
        entries
            .iter()
            .enumerate()
            .filter(|(_, (name, _, _))| query.is_empty() || name.to_lowercase().contains(&query))
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn collect_projects_for_picker(db: &Database) -> Result<Vec<ProjectPickerEntry>> {
        let mut entries = db
            .list_projects()?
            .into_iter()
            .map(|project| ProjectPickerEntry {
                hash: project.hash,
                name: project.name,
                path: project.path,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        Ok(entries)
    }

    /// Indices into `entries` whose name or path match `filter` as a
    /// case-insensitive substring — same convention as
    /// [`Self::filtered_preset_indices`], extended to path since a project's
    /// path is often what a user remembers about it.
    pub fn filtered_project_indices(entries: &[ProjectPickerEntry], filter: &str) -> Vec<usize> {
        let query = filter.trim().to_lowercase();
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.name.to_lowercase().contains(&query)
                    || entry.path.to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Height (in terminal rows, borders included) of the project picker box
    /// for `count` filtered entries: a border top/bottom, a filter row, a
    /// hint row, and up to 13 entry rows, capped so the box never grows
    /// past the screen for a large workspace. The single source of truth
    /// for both [`Self::project_picker_visible_rows`] (scrolling) and the
    /// box's rendered area, so the two can never disagree on how many rows
    /// fit.
    pub fn project_picker_box_height(count: usize) -> u16 {
        (count as u16 + 6).clamp(7, 17)
    }

    /// Number of entry rows visible inside the project picker box for
    /// `count` filtered entries — the box height minus its 2 border rows,
    /// 1 filter row, and 1 hint row.
    pub fn project_picker_visible_rows(count: usize) -> usize {
        Self::project_picker_box_height(count).saturating_sub(4) as usize
    }

    /// Box height for the Skills picker (Tools section), mirroring
    /// `project_picker_box_height`: grows to fit small lists, caps so long
    /// lists scroll instead of overflowing the screen.
    pub fn skills_picker_box_height(count: usize) -> u16 {
        (count as u16 + 5).min(16)
    }

    /// Entry rows visible inside the Skills picker box — the box height
    /// minus its 2 border rows and 1 hint row.
    pub fn skills_picker_visible_rows(count: usize) -> usize {
        Self::skills_picker_box_height(count).saturating_sub(3) as usize
    }

    /// Box height for the Remove Section picker.
    pub fn remove_section_box_height(count: usize) -> u16 {
        (count as u16 + 4).min(15)
    }

    /// Entry rows visible inside the Remove Section picker box — the box
    /// height minus its 2 border rows and 1 hint row.
    pub fn remove_section_visible_rows(count: usize) -> usize {
        Self::remove_section_box_height(count).saturating_sub(3) as usize
    }

    /// Set the content of a specific tools section to a single skill label.
    /// Used by the SkillsPicker to replace the skill in an existing tools section.
    pub fn set_tools_section_skill(&mut self, section_id: &str, label: &str) {
        self.sections
            .insert(section_id.to_string(), label.to_string());
    }

    /// Get section types available to add (can always add more instances).
    /// `Canopy protocol` is single-instance: hidden from this list once a
    /// `canopy_protocol` section is already enabled, so the picker only
    /// offers it while a resend is still possible.
    pub fn get_addable_sections(&self) -> Vec<(&'static str, &'static str)> {
        Self::get_available_sections()
            .into_iter()
            .filter(|(name, _)| *name != "canopy_protocol" || !self.has_canopy_protocol_section())
            .collect()
    }

    fn section_display_name(section_id: &str) -> String {
        let section_name = Self::section_type(section_id);
        let label = Self::get_available_sections()
            .into_iter()
            .find(|(name, _)| *name == section_name)
            .map(|(_, label)| label)
            .unwrap_or(section_name);

        if section_id.contains('_') {
            return format!("{} {}", label, section_id.rsplit('_').next().unwrap_or(""));
        }

        label.to_string()
    }

    /// Get section instances available to remove (last instruction is protected)
    pub fn get_removable_sections(&self) -> Vec<(String, String)> {
        let instruction_count = self.instruction_count();
        self.enabled_sections
            .iter()
            .filter(|section_id| {
                !Self::section_matches_prefix(section_id, "instruction") || instruction_count > 1
            })
            .map(|section_id| (section_id.clone(), Self::section_display_name(section_id)))
            .collect()
    }

    /// Get the content for a section
    pub fn get_section_content(&self, section_name: &str) -> String {
        self.sections.get(section_name).cloned().unwrap_or_default()
    }

    /// Set the content for a section
    pub fn set_section_content(&mut self, section_name: &str, content: String) {
        self.sections.insert(section_name.to_string(), content);
    }

    /// Get the real content for a section, resolving any collapsed paste.
    pub fn section_content_for_build(&self, section_id: &str) -> Option<&str> {
        self.collapsed_pastes
            .get(section_id)
            .map(|s| s.as_str())
            .or_else(|| self.sections.get(section_id).map(|s| s.as_str()))
    }

    pub fn migrate_legacy_sections(&mut self, current_project_path: Option<&str>) {
        let had_memory_context = self
            .enabled_sections
            .iter()
            .any(|section_id| Self::section_matches_prefix(section_id, "memory_context"));
        let obsolete_sections = self
            .enabled_sections
            .iter()
            .filter(|section_id| {
                Self::section_matches_prefix(section_id, "memory_context")
                    || Self::section_matches_prefix(section_id, "examples")
            })
            .cloned()
            .collect::<Vec<_>>();

        for section_id in obsolete_sections {
            self.remove_section(&section_id);
        }

        let has_project_context = self
            .enabled_sections
            .iter()
            .any(|section_id| Self::section_matches_prefix(section_id, "project_context"));
        if had_memory_context && !has_project_context {
            if let Some(project_path) = current_project_path {
                self.add_section_with_content("project_context", project_path.to_string());
            }
        }
        self.focused_section = self
            .focused_section
            .min(self.enabled_sections.len().saturating_sub(1));
    }
}

mod prompt_session;
#[cfg(test)]
mod tests;
pub use prompt_session::{PersistedBuilderState, PromptBuilderSession};
