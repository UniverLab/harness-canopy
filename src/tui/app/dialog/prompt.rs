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
            sections.push(("rag_search", "RAG Search"));
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
    pub fn collect_skills_for_picker(workdir: &std::path::Path) -> Vec<(String, String, String)> {
        let mut entries: Vec<(String, String, String)> = Vec::new();
        let project = workdir.join(".agents").join("skills");
        add_skills_from_dir(&project, "skill", &mut entries);
        if let Some(global) = dirs::home_dir().map(|h| h.join(".agents").join("skills")) {
            if global != project {
                add_skills_from_dir(&global, "global", &mut entries);
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
        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut entries: Vec<(String, String, String)> = read_dir
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("md"))
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_stem()?.to_str()?.to_string();
                let content = std::fs::read_to_string(&path).ok()?;
                let preview = content
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("")
                    .to_string();
                Some((name, preview, content))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use tempfile::tempdir;

    #[test]
    fn preset_is_discoverable_via_the_add_section_list_like_tools() {
        let dialog = SimplePromptDialog::new();
        let names: Vec<&str> = dialog
            .get_addable_sections()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(names.contains(&"preset"));
        assert!(names.contains(&"tools"));
    }

    #[test]
    fn collect_presets_for_picker_lists_md_files_sorted_with_preview() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("reviewer.md"),
            "You are the reviewer.\n\nMore text.",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("implementer.md"),
            "You are the implementer.",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored, not .md").unwrap();

        let entries = SimplePromptDialog::collect_presets_for_picker(dir.path());

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "implementer");
        assert_eq!(entries[0].1, "You are the implementer.");
        assert_eq!(entries[0].2, "You are the implementer.");
        assert_eq!(entries[1].0, "reviewer");
        assert_eq!(entries[1].1, "You are the reviewer.");
    }

    #[test]
    fn collect_presets_for_picker_returns_empty_for_missing_directory() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");

        assert!(SimplePromptDialog::collect_presets_for_picker(&missing).is_empty());
    }

    #[test]
    fn filtered_preset_indices_matches_case_insensitive_substring() {
        let entries = vec![
            (
                "implementer".to_string(),
                "preview".to_string(),
                "content".to_string(),
            ),
            (
                "reviewer".to_string(),
                "preview".to_string(),
                "content".to_string(),
            ),
            (
                "resilience".to_string(),
                "preview".to_string(),
                "content".to_string(),
            ),
        ];

        let filtered = SimplePromptDialog::filtered_preset_indices(&entries, "RE");
        let names: Vec<&str> = filtered.iter().map(|&i| entries[i].0.as_str()).collect();
        assert_eq!(names, vec!["reviewer", "resilience"]);

        assert_eq!(
            SimplePromptDialog::filtered_preset_indices(&entries, "").len(),
            3
        );
        assert!(SimplePromptDialog::filtered_preset_indices(&entries, "zzz").is_empty());
    }

    fn project_entry(name: &str, path: &str) -> ProjectPickerEntry {
        ProjectPickerEntry {
            hash: format!("{name}-hash"),
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn filtered_project_indices_matches_name_or_path_case_insensitive() {
        let entries = vec![
            project_entry("harness-canopy", "/home/user/harness-canopy"),
            project_entry("otherproj", "/home/user/OTHERPROJ"),
            project_entry("unrelated", "/srv/unrelated"),
        ];

        let by_name = SimplePromptDialog::filtered_project_indices(&entries, "HARNESS");
        assert_eq!(by_name, vec![0]);

        let by_path = SimplePromptDialog::filtered_project_indices(&entries, "otherproj");
        assert_eq!(by_path, vec![1]);

        assert_eq!(
            SimplePromptDialog::filtered_project_indices(&entries, "").len(),
            3
        );
        assert!(SimplePromptDialog::filtered_project_indices(&entries, "zzz").is_empty());
    }

    #[test]
    fn project_picker_visible_rows_caps_at_thirteen() {
        assert_eq!(SimplePromptDialog::project_picker_visible_rows(1), 3);
        assert_eq!(SimplePromptDialog::project_picker_visible_rows(11), 13);
        assert_eq!(SimplePromptDialog::project_picker_visible_rows(100), 13);
    }

    #[test]
    fn skills_picker_visible_rows_caps_at_thirteen() {
        assert_eq!(SimplePromptDialog::skills_picker_visible_rows(1), 3);
        assert_eq!(SimplePromptDialog::skills_picker_visible_rows(11), 13);
        assert_eq!(SimplePromptDialog::skills_picker_visible_rows(100), 13);
    }

    #[test]
    fn remove_section_visible_rows_caps_at_twelve() {
        assert_eq!(SimplePromptDialog::remove_section_visible_rows(1), 2);
        assert_eq!(SimplePromptDialog::remove_section_visible_rows(11), 12);
        assert_eq!(SimplePromptDialog::remove_section_visible_rows(100), 12);
    }

    #[test]
    fn build_prompt_resolves_project_context_and_file_resources() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();

        let project_dir = temp.path().join("sample-project");
        std::fs::create_dir(&project_dir).unwrap();
        let project = db.register_project_path(&project_dir).unwrap();

        let resource = project_dir.join("guide.txt");
        std::fs::write(&resource, "hello from resource").unwrap();

        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction", "do the thing".to_string());
        dialog.add_section_with_content("project_context", project.path.clone());
        dialog.add_section_with_content("resources", resource.display().to_string());

        let prompt = dialog
            .build_prompt_with_resolved_resources(&db, &project_dir)
            .unwrap();

        assert!(prompt.contains("# [PROJECT CONTEXT]: Registered Project Metadata"));
        assert!(prompt.contains(&project.hash));
        assert!(prompt.contains("kind: file"));
        assert!(prompt.contains("guide.txt"));
    }

    #[test]
    fn build_prompt_resolves_project_directory_in_resources() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();

        let project_dir = temp.path().join("dir-project");
        std::fs::create_dir(&project_dir).unwrap();
        let project = db.register_project_path(&project_dir).unwrap();

        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction", "summarize".to_string());
        dialog.add_section_with_content("resources", project.path.clone());

        let prompt = dialog
            .build_prompt_with_resolved_resources(&db, &project_dir)
            .unwrap();

        assert!(prompt.contains("workdir_hash:"));
        assert!(prompt.contains(&project.hash));
    }

    #[test]
    fn build_prompt_auto_injects_project_context_for_resource_descendant() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();

        let project_dir = temp.path().join("linked-project");
        std::fs::create_dir(&project_dir).unwrap();
        let project = db.register_project_path(&project_dir).unwrap();

        let nested_dir = project_dir.join("src").join("module");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let file_path = nested_dir.join("lib.rs");
        std::fs::write(&file_path, "pub fn demo() {}").unwrap();

        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "summarize".to_string());
        dialog.add_section_with_content("resources", file_path.display().to_string());

        let prompt = dialog
            .build_prompt_with_resolved_resources(&db, temp.path())
            .unwrap();

        assert!(prompt.contains("# [PROJECT CONTEXT]: Registered Project Metadata"));
        assert!(prompt.contains(&project.hash));
        assert!(prompt.contains("kind: file"));
        assert!(prompt.contains("lib.rs"));
    }

    #[test]
    fn system_block_leads_the_prompt() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();
        let project_dir = temp.path().join("proj");
        std::fs::create_dir(&project_dir).unwrap();

        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "do it".to_string());
        dialog.system_content = Some("[CANOPY] call get_tools".to_string());

        let prompt = dialog
            .build_prompt_with_resolved_resources(&db, &project_dir)
            .unwrap();

        assert!(prompt.starts_with("<system>\n"));
        let system_end = prompt.find("</system>").expect("system block present");
        let instructions = prompt
            .find("# [INSTRUCTIONS]")
            .expect("instructions present");
        // The whole system block must come before the task instructions.
        assert!(system_end < instructions);
    }

    #[test]
    fn migrate_legacy_sections_replaces_memory_with_project_context() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section_with_content("memory_context", "legacy".to_string());
        dialog.add_section_with_content("examples", "old".to_string());

        dialog.migrate_legacy_sections(Some("/tmp/project"));

        assert!(dialog
            .enabled_sections
            .iter()
            .all(|section_id| !section_id.starts_with("memory_context_")
                && !section_id.starts_with("examples_")));
        assert!(dialog
            .enabled_sections
            .iter()
            .any(|section_id| section_id.starts_with("project_context_")));
    }

    #[test]
    fn move_cursor_vertical_respects_hard_newlines() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "abc\ndefgh\nij".to_string());
        // Cursor on "defgh" line, column 4 (after 'g': indices a=0..c=2,\n=3,d=4..h=8)
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 8);

        dialog.move_cursor_up("instruction_1", 40);
        // First line only has 3 columns; cursor clamps to its end (index 3, col 3).
        assert_eq!(dialog.cursor("instruction_1"), 3);

        dialog.move_cursor_down("instruction_1", 40);
        // Back down to "defgh" at column 3 → index 7.
        assert_eq!(dialog.cursor("instruction_1"), 7);

        dialog.move_cursor_down("instruction_1", 40);
        // "ij" line, column 2 max → index 12 (end of text).
        assert_eq!(dialog.cursor("instruction_1"), 12);

        // Down on the last line jumps to end; up from the first line jumps to 0.
        dialog.move_cursor_down("instruction_1", 40);
        assert_eq!(dialog.cursor("instruction_1"), 12);
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 1);
        dialog.move_cursor_up("instruction_1", 40);
        assert_eq!(dialog.cursor("instruction_1"), 0);
    }

    #[test]
    fn move_cursor_vertical_handles_soft_wrap() {
        let mut dialog = SimplePromptDialog::new();
        // width 5: "aaaaa" | "bbbbb" as two visual lines, no '\n' present
        dialog.set_section_content("instruction_1", "aaaaabbbbb".to_string());
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 8);

        dialog.move_cursor_up("instruction_1", 5);
        // Same column (3) on the first visual line → index 3.
        assert_eq!(dialog.cursor("instruction_1"), 3);

        dialog.move_cursor_down("instruction_1", 5);
        assert_eq!(dialog.cursor("instruction_1"), 8);
    }

    #[test]
    fn test_non_ascii_handling_does_not_panic() {
        // Typing past the field width must never mutate the stored content:
        // soft wrapping is a render-time concern only.
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "áéíóú ".to_string());
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 6);
        for ch in "world".chars() {
            dialog.insert_char_at_cursor("instruction_1", ch, 10);
        }
        assert_eq!(dialog.get_section_content("instruction_1"), "áéíóú world");
        assert_eq!(dialog.cursor("instruction_1"), 11);
    }

    #[test]
    fn test_collapsed_placeholder_non_ascii_indices() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "áéíóú [Pasted ~5 lines] extra".to_string());
        dialog.collapsed_pastes.insert(
            "instruction_1".to_string(),
            "original content\nwith multiple lines".to_string(),
        );

        // Character indices:
        // "áéíóú " is 6 characters.
        // "[Pasted ~5 lines]" is 18 characters.
        // "start" char of placeholder is 6.
        // "end" char of placeholder is 6 + 18 = 24.

        // Let's test cursor_in_collapsed_placeholder
        // Cursor at 5 (on the space) -> false
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 5);
        assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));

        // Cursor at 7 (inside placeholder) -> true
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 7);
        assert!(dialog.cursor_in_collapsed_placeholder("instruction_1"));

        // Cursor at 23 (on ']') -> true
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 23);
        assert!(dialog.cursor_in_collapsed_placeholder("instruction_1"));

        // Cursor at 24 (after placeholder) -> false
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 24);
        assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));

        // Test backspace_collapsed_paste with cursor inside placeholder
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 15);
        dialog.backspace_collapsed_paste("instruction_1", 80);
        // It should delete the placeholder "[Pasted ~5 lines]" and leave "áéíóú  extra"
        assert_eq!(dialog.get_section_content("instruction_1"), "áéíóú  extra");
        // Cursor should be at 6 (the start of deleted placeholder)
        assert_eq!(dialog.cursor("instruction_1"), 6);
    }

    #[test]
    fn insert_collapsed_paste_at_cursor_collapses_multiline_paste() {
        let mut dialog = SimplePromptDialog::new();
        let pasted = "line one\nline two\nline three";

        dialog.insert_collapsed_paste_at_cursor("instruction_1", pasted, 80);

        let displayed = dialog.get_section_content("instruction_1");
        assert!(displayed.contains("[Pasted ~3 lines]"));
        assert!(!displayed.contains("line one"));
        // The full pasted text (all 3 lines) is still what gets sent to the CLI.
        assert_eq!(
            dialog.section_content_for_build("instruction_1"),
            Some(pasted)
        );
    }

    #[test]
    fn insert_at_completion_removes_leftover_manual_at_before_trigger() {
        let temp = tempdir().unwrap();
        let workdir = temp.path().to_path_buf();

        let mut dialog = SimplePromptDialog::new();
        // Simulate: user typed "@" (left over from a dismissed picker), then
        // typed "@" again right after it — trigger_pos points at the second "@".
        dialog.set_section_content("instruction_1", "look @@".to_string());
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 7);
        dialog.at_picker = Some(AtPicker::new(workdir, 6));

        dialog.insert_at_completion("instruction_1", "src/lib.rs", "/abs/src/lib.rs", 80);

        assert_eq!(
            dialog.get_section_content("instruction_1"),
            "look @src/lib.rs"
        );
    }

    #[test]
    fn is_empty_true_for_fresh_dialog_and_false_once_filled() {
        let mut dialog = SimplePromptDialog::new();
        assert!(dialog.is_empty());

        dialog.set_section_content("instruction_1", "do the thing".to_string());
        assert!(!dialog.is_empty());
    }

    #[test]
    fn is_empty_true_when_sections_are_only_whitespace() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "   \n  ".to_string());
        dialog.add_section_with_content("context", "  ".to_string());
        assert!(dialog.is_empty());
    }

    #[test]
    fn load_flat_text_replaces_all_content_with_a_single_instruction() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "stale draft".to_string());
        dialog.add_section_with_content("context", "stale context".to_string());
        dialog.lock_section("context_1");

        dialog.load_flat_text("recovered prompt text");

        assert_eq!(dialog.enabled_sections, vec!["instruction_1".to_string()]);
        assert_eq!(
            dialog.get_section_content("instruction_1"),
            "recovered prompt text"
        );
        assert!(dialog.locked_sections.is_empty());
        assert_eq!(
            dialog.cursor("instruction_1"),
            "recovered prompt text".chars().count()
        );
    }

    #[test]
    fn persisted_builder_state_round_trips_through_json() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "ship the feature".to_string());
        dialog.add_section_with_content("tools", "skill:code-engineering".to_string());
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(
            chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
                .unwrap()
                .and_hms_opt(14, 30, 0)
                .unwrap(),
        );

        let snapshot = PersistedBuilderState::from_dialog(&dialog);
        let json = serde_json::to_string(&snapshot).expect("serialize");
        let restored: PersistedBuilderState = serde_json::from_str(&json).expect("deserialize");

        let mut target = SimplePromptDialog::new();
        restored.restore_into(&mut target);

        assert_eq!(
            target.get_section_content("instruction_1"),
            "ship the feature"
        );
        assert_eq!(
            target.get_section_content("tools_1"),
            "skill:code-engineering"
        );
        // send_at is intentionally not part of the snapshot — recall must not
        // resurrect a stale schedule.
        assert!(target.send_at.is_none());
    }

    #[test]
    fn for_instruction_prompt_restores_into_a_normal_tab_dialog() {
        let prompt = "Graph nightly failed: build broke";
        let state = PersistedBuilderState::for_instruction_prompt(prompt);
        let json = serde_json::to_string(&state).expect("serialize");
        let restored: PersistedBuilderState = serde_json::from_str(&json).expect("deserialize");

        let mut target = SimplePromptDialog::new();
        restored.restore_into(&mut target);

        assert_eq!(target.get_section_content("instruction_1"), prompt);
        assert_eq!(target.enabled_sections, vec!["instruction_1".to_string()]);
        // Normal-tab structure: no schedule re-armed, empty collapse/lock maps.
        assert!(target.send_at.is_none());
        assert!(target.collapsed_pastes.is_empty());
        assert!(target.locked_sections.is_empty());
        assert_eq!(target.active_tab, PromptTab::Normal);
    }

    #[test]
    fn add_section_focuses_the_new_section_with_cursor_at_start() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("goal");

        // Focus must land IN the newly created section, ready to type.
        let focused = dialog
            .focused_section_name()
            .expect("a section is focused, not the send control")
            .to_string();
        assert!(
            focused.starts_with("goal"),
            "expected focus on the new goal section, got {focused}"
        );
        // Cursor sits at position 0 of the empty new section.
        assert_eq!(dialog.cursor(&focused), 0);

        // A second added section also grabs focus (not the previous one).
        dialog.add_section("constraints");
        let focused = dialog.focused_section_name().unwrap().to_string();
        assert!(focused.starts_with("constraints"));
        assert_eq!(dialog.cursor(&focused), 0);
    }

    #[test]
    fn new_dialog_focuses_the_first_section() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.focused_section, 1);
        assert_eq!(dialog.focused_section_name(), Some("instruction_1"));
    }

    #[test]
    fn restored_session_focuses_first_section_regardless_of_persisted_focus() {
        let mut source = SimplePromptDialog::new();
        source.add_section("goal");
        // Pretend the user left focus parked on the send control (index 0).
        source.focused_section = 0;
        let session = PromptBuilderSession::from_dialog(&source);

        let mut target = SimplePromptDialog::new();
        session.restore_into(&mut target);
        assert_eq!(target.focused_section, 1);
        assert!(target.focused_section_name().is_some());
    }

    #[test]
    fn recalled_persisted_state_focuses_first_section_regardless_of_persisted_focus() {
        let mut source = SimplePromptDialog::new();
        source.focused_section = 0; // persisted on the send control
        let snapshot = PersistedBuilderState::from_dialog(&source);

        let mut target = SimplePromptDialog::new();
        target.focused_section = 0;
        snapshot.restore_into(&mut target);
        assert_eq!(target.focused_section, 1);
        assert_eq!(target.focused_section_name(), Some("instruction_1"));
    }

    // ── Tabs & Raw mode ─────────────────────────────────────────────────

    #[test]
    fn toggle_tab_flips_active_tab_and_focus() {
        let mut dialog = SimplePromptDialog::new();
        assert_eq!(dialog.active_tab, PromptTab::Normal);
        assert_eq!(dialog.focused_section, 1);

        dialog.toggle_tab();
        assert_eq!(dialog.active_tab, PromptTab::Raw);
        // Raw lands focus on the raw buffer (index 1), not the send control.
        assert_eq!(dialog.focused_section, 1);

        dialog.toggle_tab();
        assert_eq!(dialog.active_tab, PromptTab::Normal);
        assert_eq!(dialog.focused_section, 1);
    }

    #[test]
    fn tab_at_maps_clicks_to_the_right_tab() {
        // Tab bar origin at (2, 1). " Normal " spans cols 2..9, " Raw " 10..14.
        let x = 2;
        let y = 1;
        assert_eq!(
            SimplePromptDialog::tab_at(x, y, 3, 1),
            Some(PromptTab::Normal)
        );
        assert_eq!(
            SimplePromptDialog::tab_at(x, y, 9, 1),
            Some(PromptTab::Normal)
        );
        assert_eq!(
            SimplePromptDialog::tab_at(x, y, 10, 1),
            Some(PromptTab::Raw)
        );
        assert_eq!(
            SimplePromptDialog::tab_at(x, y, 13, 1),
            Some(PromptTab::Raw)
        );
        // Left of the bar, right of the bar, and a different row all miss.
        assert_eq!(SimplePromptDialog::tab_at(x, y, 1, 1), None);
        assert_eq!(SimplePromptDialog::tab_at(x, y, 20, 1), None);
        assert_eq!(SimplePromptDialog::tab_at(x, y, 3, 2), None);
    }

    #[test]
    fn raw_non_empty_send_produces_exactly_the_raw_text() {
        let temp = tempdir().unwrap();
        let db = Database::new(&temp.path().join("canopy.db")).unwrap();
        let workdir = temp.path().to_path_buf();

        let mut dialog = SimplePromptDialog::new();
        // Non-raw content that WOULD be composed if we were on the Normal tab —
        // proves the raw path bypasses XML/system composition entirely.
        dialog.set_section_content("instruction_1", "compose me".to_string());
        dialog.system_content = Some("SYSTEM PROTOCOL".to_string());
        dialog.set_tab(PromptTab::Raw);
        dialog
            .sections
            .insert(RAW_SECTION_ID.to_string(), "/compact".to_string());

        let out = dialog.resolve_outgoing_prompt(&db, &workdir).unwrap();
        assert_eq!(out, "/compact");
        assert!(!out.contains("SYSTEM"));
        assert!(!out.contains("[INSTRUCTIONS]"));
    }

    #[test]
    fn raw_empty_preview_and_send_equal_the_composed_prompt() {
        let temp = tempdir().unwrap();
        let db = Database::new(&temp.path().join("canopy.db")).unwrap();
        let workdir = temp.path().to_path_buf();

        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "do the thing".to_string());
        let composed = dialog
            .build_prompt_with_resolved_resources(&db, &workdir)
            .unwrap();

        dialog.set_tab(PromptTab::Raw);
        assert!(dialog.raw_is_empty());

        // Sending from the empty Raw tab sends the composed prompt verbatim.
        let out = dialog.resolve_outgoing_prompt(&db, &workdir).unwrap();
        assert_eq!(out, composed);

        // The cached preview shows exactly that same composed string.
        dialog.refresh_raw_preview(&db, &workdir);
        assert_eq!(dialog.raw_preview.as_deref(), Some(composed.as_str()));
    }

    // ── Scheduled-sends list panel (B33) ────────────────────────────────

    #[test]
    fn scheduled_list_view_no_scroll_when_all_rows_fit() {
        // 3 entries, panel shows up to 4 → everything visible, never scrolled.
        assert_eq!(
            SimplePromptDialog::scheduled_list_view(3, 4, Some(2)),
            (3, 0)
        );
        assert_eq!(SimplePromptDialog::scheduled_list_view(3, 4, None), (3, 0));
    }

    #[test]
    fn scheduled_list_view_scrolls_to_keep_selection_in_view() {
        // 6 entries, 3 visible rows. Selecting row 4 scrolls so it is the last
        // visible row (offset 2 → rows 2,3,4).
        assert_eq!(
            SimplePromptDialog::scheduled_list_view(6, 3, Some(4)),
            (3, 2)
        );
        // The last row never scrolls past the end (offset clamps to total-visible).
        assert_eq!(
            SimplePromptDialog::scheduled_list_view(6, 3, Some(5)),
            (3, 3)
        );
        // Early rows keep the panel pinned to the top.
        assert_eq!(
            SimplePromptDialog::scheduled_list_view(6, 3, Some(1)),
            (3, 0)
        );
    }

    #[test]
    fn scheduled_list_view_empty_is_zero() {
        assert_eq!(SimplePromptDialog::scheduled_list_view(0, 4, None), (0, 0));
        assert_eq!(
            SimplePromptDialog::scheduled_list_view(5, 0, Some(1)),
            (0, 0)
        );
    }

    #[test]
    fn load_scheduled_for_edit_loads_raw_and_records_editing_id() {
        let mut dialog = SimplePromptDialog::new();
        // Some pre-existing Normal-form content that must be bypassed.
        dialog.set_section_content("instruction_1", "compose me".to_string());
        dialog.scheduled_list_selected = Some(2);

        let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 2)
            .unwrap()
            .and_hms_opt(9, 15, 0)
            .unwrap();
        dialog.load_scheduled_for_edit("ss-42", "deliver this later", fire, None);

        assert_eq!(dialog.active_tab, PromptTab::Raw);
        assert_eq!(dialog.raw_text(), "deliver this later");
        assert_eq!(
            dialog.cursor(RAW_SECTION_ID),
            "deliver this later".chars().count()
        );
        assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-42"));
        assert_eq!(dialog.send_choice, SendChoice::Date);
        assert_eq!(dialog.send_at, Some(fire));
        // Focus lands on the raw buffer, and the list focus is released.
        assert_eq!(dialog.focused_section, 1);
        assert!(dialog.scheduled_list_selected.is_none());
    }

    #[test]
    fn load_scheduled_for_edit_with_builder_state_restores_structured_view() {
        let mut source = SimplePromptDialog::new();
        source.set_tab(PromptTab::Normal);
        source.set_section_content("instruction_1", "do the thing".to_string());
        source.set_section_content("context_1", "some context".to_string());

        let snapshot = PersistedBuilderState::from_dialog(&source);
        let json = serde_json::to_string(&snapshot).unwrap();

        let mut dialog = SimplePromptDialog::new();
        let fire = chrono::NaiveDate::from_ymd_opt(2030, 6, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        dialog.load_scheduled_for_edit(
            "ss-struct",
            "ignored when state present",
            fire,
            Some(&json),
        );

        assert_eq!(dialog.active_tab, PromptTab::Normal);
        assert_eq!(
            dialog.sections.get("instruction_1").map(|s| s.as_str()),
            Some("do the thing")
        );
        assert_eq!(
            dialog.sections.get("context_1").map(|s| s.as_str()),
            Some("some context")
        );
        assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-struct"));
        assert_eq!(dialog.send_choice, SendChoice::Date);
        assert_eq!(dialog.send_at, Some(fire));
        assert!(dialog.scheduled_list_selected.is_none());
    }

    #[test]
    fn load_scheduled_for_edit_with_none_builder_state_falls_back_to_raw() {
        let mut dialog = SimplePromptDialog::new();
        let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        dialog.load_scheduled_for_edit("ss-old", "plain text prompt", fire, None);

        assert_eq!(dialog.active_tab, PromptTab::Raw);
        assert_eq!(dialog.raw_text(), "plain text prompt");
        assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-old"));
    }

    #[test]
    fn load_scheduled_for_edit_with_invalid_json_falls_back_to_raw() {
        let mut dialog = SimplePromptDialog::new();
        let fire = chrono::NaiveDate::from_ymd_opt(2030, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        dialog.load_scheduled_for_edit("ss-bad", "fallback text", fire, Some("not valid json{{{"));

        assert_eq!(dialog.active_tab, PromptTab::Raw);
        assert_eq!(dialog.raw_text(), "fallback text");
        assert_eq!(dialog.editing_scheduled_id.as_deref(), Some("ss-bad"));
    }

    #[test]
    fn raw_buffer_and_active_tab_survive_session_round_trip() {
        let mut source = SimplePromptDialog::new();
        source.set_tab(PromptTab::Raw);
        source.sections.insert(
            RAW_SECTION_ID.to_string(),
            "/compact when quota back".to_string(),
        );

        let session = PromptBuilderSession::from_dialog(&source);
        let mut target = SimplePromptDialog::new();
        session.restore_into(&mut target);

        assert_eq!(target.active_tab, PromptTab::Raw);
        assert_eq!(target.raw_text(), "/compact when quota back");
        // Restoring the Raw tab lands focus on the buffer, not the send control.
        assert_eq!(target.focused_section, 1);
    }

    // ── Raw-tab mouse-wheel scrolling (U13) ────────────────────────────────

    #[test]
    fn raw_content_rect_matches_draw_geometry() {
        // A typical inner rect (border excluded). The content area should
        // start 3 rows below inner.y (tab bar + hint + gap) and end 2 rows
        // above inner.bottom (send line + gap), minus the list panel.
        let inner = ratatui::layout::Rect::new(5, 3, 60, 30);
        let rect = SimplePromptDialog::raw_content_rect(inner, 0);
        assert_eq!(rect.x, 6); // inner.x + 1
        assert_eq!(rect.y, 6); // inner.y + 3
        assert_eq!(rect.width, 58); // inner.width - 2
                                    // height = inner.height - 2 (send+gap) - 3 (top) = 30 - 5 = 25
        assert_eq!(rect.height, 25);
    }

    #[test]
    fn raw_content_rect_with_list_panel() {
        let inner = ratatui::layout::Rect::new(0, 0, 60, 20);
        let rect = SimplePromptDialog::raw_content_rect(inner, 3);
        // bottom = 20 - (2 + 3) = 15; top = 3; height = 15 - 3 = 12
        assert_eq!(rect.height, 12);
    }

    #[test]
    fn raw_content_rect_small_dialog() {
        // Minimum-size dialog: inner height 6 (borders + tab + hint + gap + content + gap + send).
        let inner = ratatui::layout::Rect::new(0, 0, 40, 6);
        let rect = SimplePromptDialog::raw_content_rect(inner, 0);
        // bottom = 6 - 2 = 4; top = 3; height = max(4-3, 1) = 1
        assert_eq!(rect.height, 1);
        assert!(rect.width > 0);
    }

    #[test]
    fn scroll_raw_edit_clamps_at_zero() {
        let mut dialog = SimplePromptDialog::new();
        assert!(dialog.raw_edit_scroll.is_none());
        dialog.scroll_raw_edit(-5, 100, 20);
        // Clamped to 0 (cannot scroll above the first line).
        assert_eq!(dialog.raw_edit_scroll, Some(0));
    }

    #[test]
    fn scroll_raw_edit_clamps_at_max() {
        let mut dialog = SimplePromptDialog::new();
        // total_lines = 50, avail_h = 10 → max_scroll = 40
        dialog.scroll_raw_edit(100, 50, 10);
        assert_eq!(dialog.raw_edit_scroll, Some(40));
    }

    #[test]
    fn scroll_raw_edit_accumulates() {
        let mut dialog = SimplePromptDialog::new();
        dialog.scroll_raw_edit(3, 100, 20);
        assert_eq!(dialog.raw_edit_scroll, Some(3));
        dialog.scroll_raw_edit(2, 100, 20);
        assert_eq!(dialog.raw_edit_scroll, Some(5));
        dialog.scroll_raw_edit(-1, 100, 20);
        assert_eq!(dialog.raw_edit_scroll, Some(4));
    }

    #[test]
    fn raw_edit_scroll_cleared_on_tab_switch() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_tab(PromptTab::Raw);
        dialog.raw_edit_scroll = Some(5);
        dialog.set_tab(PromptTab::Normal);
        assert!(dialog.raw_edit_scroll.is_none());
        dialog.set_tab(PromptTab::Raw);
        assert!(dialog.raw_edit_scroll.is_none());
    }

    #[test]
    fn raw_edit_scroll_initially_none() {
        let dialog = SimplePromptDialog::new();
        assert!(dialog.raw_edit_scroll.is_none());
    }

    // ── field_digit_width ────────────────────────────────────────

    #[test]
    fn field_digit_width_year_is_4() {
        assert_eq!(field_digit_width(0), 4);
    }

    #[test]
    fn field_digit_width_month_is_2() {
        assert_eq!(field_digit_width(1), 2);
    }

    #[test]
    fn field_digit_width_day_is_2() {
        assert_eq!(field_digit_width(2), 2);
    }

    #[test]
    fn field_digit_width_hour_is_2() {
        assert_eq!(field_digit_width(3), 2);
    }

    #[test]
    fn field_digit_width_minute_is_2() {
        assert_eq!(field_digit_width(4), 2);
    }

    // ── days_in_month ────────────────────────────────────────────

    #[test]
    fn days_in_month_january() {
        assert_eq!(days_in_month(2024, 1), 31);
    }

    #[test]
    fn days_in_month_february_leap_year() {
        assert_eq!(days_in_month(2024, 2), 29);
    }

    #[test]
    fn days_in_month_february_non_leap() {
        assert_eq!(days_in_month(2023, 2), 28);
    }

    #[test]
    fn days_in_month_april() {
        assert_eq!(days_in_month(2024, 4), 30);
    }

    #[test]
    fn days_in_month_december() {
        assert_eq!(days_in_month(2024, 12), 31);
    }

    // ── add_months ───────────────────────────────────────────────

    #[test]
    fn add_months_forward() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = add_months(dt, 3).unwrap();
        assert_eq!(result.month(), 4);
        assert_eq!(result.year(), 2024);
    }

    #[test]
    fn add_months_backward() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 3, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = add_months(dt, -1).unwrap();
        assert_eq!(result.month(), 2);
    }

    #[test]
    fn add_months_year_rollover() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 11, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = add_months(dt, 2).unwrap();
        assert_eq!(result.month(), 1);
        assert_eq!(result.year(), 2025);
    }

    #[test]
    fn add_months_day_clamping() {
        // Jan 31 + 1 month = Feb 29 (2024 is leap)
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let result = add_months(dt, 1).unwrap();
        assert_eq!(result.month(), 2);
        assert_eq!(result.day(), 29);
    }

    // ── with_field ───────────────────────────────────────────────

    #[test]
    fn with_field_year() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = with_field(dt, 0, 2030).unwrap();
        assert_eq!(result.year(), 2030);
        assert_eq!(result.month(), 6);
        assert_eq!(result.day(), 15);
    }

    #[test]
    fn with_field_month_clamps_day() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 1, 31)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = with_field(dt, 1, 2).unwrap();
        assert_eq!(result.month(), 2);
        assert_eq!(result.day(), 29); // Clamped to Feb 29 (leap year)
    }

    #[test]
    fn with_field_day_clamped_to_month_length() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 2, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = with_field(dt, 2, 31).unwrap();
        assert_eq!(result.day(), 29); // Feb 2024 has 29 days
    }

    #[test]
    fn with_field_hour_clamped() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = with_field(dt, 3, 25).unwrap();
        assert_eq!(result.hour(), 23);
    }

    #[test]
    fn with_field_minute_clamped() {
        let dt = chrono::NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap();
        let result = with_field(dt, 4, 65).unwrap();
        assert_eq!(result.minute(), 59);
    }

    // ── strip_resources_section ──────────────────────────────────

    #[test]
    fn strip_resources_section_removes_present_section() {
        let input = "before\n# [RESOURCES]: Knowledge Base & Data\n<resources>\n  <resource>\n    path: /foo\n  </resource>\n</resources>\n\nafter";
        let result = strip_resources_section(input);
        assert!(result.starts_with("before"));
        assert!(result.ends_with("after"));
        assert!(!result.contains("RESOURCES"));
    }

    #[test]
    fn strip_resources_section_no_section_unchanged() {
        let input = "no resources section here";
        assert_eq!(strip_resources_section(input), input);
    }

    #[test]
    fn strip_resources_section_missing_closing_tag() {
        let input = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n  content\n";
        assert_eq!(strip_resources_section(input), input);
    }

    // ── format_indexed_xml_section ────────────────────────────────

    #[test]
    fn format_indexed_xml_section_empty_items() {
        assert!(format_indexed_xml_section("header", "outer", "item", &[]).is_none());
    }

    #[test]
    fn format_indexed_xml_section_with_items() {
        let items = vec!["item1".to_string(), "item2".to_string()];
        let result = format_indexed_xml_section("# Header\n", "outer", "item", &items).unwrap();
        assert!(result.contains("# Header"));
        assert!(result.contains("<outer>"));
        assert!(result.contains("</outer>"));
        assert!(result.contains("<item>"));
        assert!(result.contains("item1"));
        assert!(result.contains("item2"));
    }

    // ── push_xml_item ────────────────────────────────────────────

    #[test]
    fn push_xml_item_basic() {
        let mut result = String::new();
        push_xml_item(&mut result, "tag", "content here");
        assert!(result.contains("<tag>"));
        assert!(result.contains("content here"));
        assert!(result.contains("</tag>"));
    }

    #[test]
    fn push_xml_item_multiline() {
        let mut result = String::new();
        push_xml_item(&mut result, "tag", "line1\nline2");
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
    }

    // ── append_tool_skill ────────────────────────────────────────

    #[test]
    fn append_tool_skill_basic() {
        let mut result = String::new();
        append_tool_skill(&mut result, "skill:code-engineering");
        assert!(result.contains("<skill>"));
        assert!(result.contains("skill:code-engineering"));
        assert!(result.contains("</skill>"));
    }

    // ── visual_line_count edge cases ─────────────────────────────

    #[test]
    fn visual_line_count_empty_string() {
        assert_eq!(SimplePromptDialog::visual_line_count("", 40), 1);
    }

    #[test]
    fn visual_line_count_single_line() {
        assert_eq!(SimplePromptDialog::visual_line_count("hello", 40), 1);
    }

    #[test]
    fn visual_line_count_zero_width() {
        assert_eq!(SimplePromptDialog::visual_line_count("hello", 0), 1);
    }

    #[test]
    fn visual_line_count_newlines() {
        assert_eq!(SimplePromptDialog::visual_line_count("a\nb\nc", 40), 3);
    }

    #[test]
    fn visual_line_count_soft_wrap() {
        // 10 chars in a field_width of 5 → 2 visual lines
        assert_eq!(SimplePromptDialog::visual_line_count("abcdefghij", 5), 2);
    }

    #[test]
    fn visual_line_count_tabs() {
        // Tab at col 0 takes 4 cols, 'x' at col 4 within width 5 → 1 line total
        assert_eq!(SimplePromptDialog::visual_line_count("\tx", 5), 1);
    }

    #[test]
    fn visual_line_count_tab_no_wrap() {
        // Tab at col 0 takes 4 cols, then "ab" (2 chars) → col 6, within width 10
        assert_eq!(SimplePromptDialog::visual_line_count("\tab", 10), 1);
    }

    // ── max_visible_lines ────────────────────────────────────────

    #[test]
    fn max_visible_lines_instruction_section() {
        assert_eq!(SimplePromptDialog::max_visible_lines("instruction_1"), 5);
    }

    #[test]
    fn max_visible_lines_other_section() {
        assert_eq!(SimplePromptDialog::max_visible_lines("context_1"), 3);
        assert_eq!(SimplePromptDialog::max_visible_lines("goal_1"), 3);
        assert_eq!(SimplePromptDialog::max_visible_lines("tools_1"), 3);
    }

    // ── section_type ─────────────────────────────────────────────

    #[test]
    fn section_type_instruction() {
        assert_eq!(
            SimplePromptDialog::section_type("instruction_1"),
            "instruction"
        );
        assert_eq!(
            SimplePromptDialog::section_type("instruction"),
            "instruction"
        );
    }

    #[test]
    fn section_type_context() {
        assert_eq!(SimplePromptDialog::section_type("context_2"), "context");
    }

    #[test]
    fn section_type_unknown() {
        assert_eq!(
            SimplePromptDialog::section_type("custom_thing"),
            "custom_thing"
        );
    }

    // ── section_matches_prefix ───────────────────────────────────

    #[test]
    fn section_matches_prefix_exact() {
        assert!(SimplePromptDialog::section_matches_prefix(
            "instruction",
            "instruction"
        ));
    }

    #[test]
    fn section_matches_prefix_with_suffix() {
        assert!(SimplePromptDialog::section_matches_prefix(
            "instruction_1",
            "instruction"
        ));
    }

    #[test]
    fn section_matches_prefix_no_match() {
        assert!(!SimplePromptDialog::section_matches_prefix(
            "context_1",
            "instruction"
        ));
    }

    // ── is_tools_section ─────────────────────────────────────────

    #[test]
    fn is_tools_section_exact() {
        assert!(SimplePromptDialog::is_tools_section("tools"));
    }

    #[test]
    fn is_tools_section_with_suffix() {
        assert!(SimplePromptDialog::is_tools_section("tools_1"));
    }

    #[test]
    fn is_tools_section_no_match() {
        assert!(!SimplePromptDialog::is_tools_section("context"));
        assert!(!SimplePromptDialog::is_tools_section("tool"));
    }

    // ── is_file_reference ────────────────────────────────────────

    #[test]
    fn is_file_reference_valid() {
        assert!(SimplePromptDialog::is_file_reference("@src/lib.rs"));
        assert!(SimplePromptDialog::is_file_reference("@file.txt"));
        assert!(SimplePromptDialog::is_file_reference("@a"));
    }

    #[test]
    fn is_file_reference_single_at() {
        assert!(!SimplePromptDialog::is_file_reference("@"));
    }

    #[test]
    fn is_file_reference_special_chars() {
        assert!(!SimplePromptDialog::is_file_reference("@file with spaces"));
        assert!(SimplePromptDialog::is_file_reference("@file_name-1.0.rs"));
    }

    // ── resolve_rag_scope ────────────────────────────────────────

    #[test]
    fn resolve_rag_scope_global_prefix() {
        let (scope, query) = SimplePromptDialog::resolve_rag_scope("global:my query", None);
        assert!(matches!(scope, RagScope::Global));
        assert_eq!(query, "my query");
    }

    #[test]
    fn resolve_rag_scope_project_prefix() {
        let (scope, query) = SimplePromptDialog::resolve_rag_scope("project:abc123:my query", None);
        assert!(matches!(scope, RagScope::Project("abc123")));
        assert_eq!(query, "my query");
    }

    #[test]
    fn resolve_rag_scope_no_prefix_with_default() {
        let (scope, query) = SimplePromptDialog::resolve_rag_scope("my query", Some("hash1"));
        assert!(matches!(scope, RagScope::Project("hash1")));
        assert_eq!(query, "my query");
    }

    #[test]
    fn resolve_rag_scope_no_prefix_no_default() {
        let (scope, query) = SimplePromptDialog::resolve_rag_scope("my query", None);
        assert!(matches!(scope, RagScope::Global));
        assert_eq!(query, "my query");
    }

    // ── next_file_reference ──────────────────────────────────────

    #[test]
    fn next_file_reference_finds_at() {
        let text = "look at @src/lib.rs for details";
        let result = SimplePromptDialog::next_file_reference(text, 0);
        assert!(result.is_some());
        let (pos, _ref, next) = result.unwrap();
        assert_eq!(pos, 8); // "look at @" — @ is at index 8
        assert_eq!(_ref, "@src/lib.rs");
        assert!(next > pos);
    }

    #[test]
    fn next_file_reference_no_at() {
        let text = "no references here";
        assert!(SimplePromptDialog::next_file_reference(text, 0).is_none());
    }

    #[test]
    fn next_file_reference_with_offset() {
        let text = "first @one then @two";
        let (pos1, ref1, next1) = SimplePromptDialog::next_file_reference(text, 0).unwrap();
        assert_eq!(pos1, 6);
        assert_eq!(ref1, "@one");
        let (pos2, ref2, _) = SimplePromptDialog::next_file_reference(text, next1).unwrap();
        assert_eq!(pos2, 16);
        assert_eq!(ref2, "@two");
    }

    // ── focus navigation ─────────────────────────────────────────

    #[test]
    fn focus_next_wraps_from_last_to_send() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("goal");
        let last = dialog.enabled_sections.len();
        dialog.focused_section = last; // last section
        dialog.focus_next();
        assert_eq!(dialog.focused_section, 0); // send control
    }

    #[test]
    fn focus_next_wraps_from_send_to_first() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 0;
        dialog.focus_next();
        assert_eq!(dialog.focused_section, 1);
    }

    #[test]
    fn focus_prev_from_send_goes_to_last() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("goal");
        dialog.focused_section = 0;
        dialog.focus_prev();
        assert_eq!(dialog.focused_section, dialog.enabled_sections.len());
    }

    #[test]
    fn focus_prev_from_first_goes_to_send() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 1;
        dialog.focus_prev();
        assert_eq!(dialog.focused_section, 0);
    }

    #[test]
    fn focused_section_index_send_returns_none() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 0;
        assert!(dialog.focused_section_index().is_none());
    }

    #[test]
    fn focused_section_index_section_returns_some() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.focused_section_index(), Some(0));
    }

    // ── send display ─────────────────────────────────────────────

    #[test]
    fn send_display_now() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "now");
    }

    #[test]
    fn send_display_date_no_time() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_choice = SendChoice::Date;
        assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "date");
    }

    #[test]
    fn send_display_date_with_time() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(
            chrono::NaiveDate::from_ymd_opt(2026, 7, 20)
                .unwrap()
                .and_hms_opt(14, 30, 0)
                .unwrap(),
        );
        assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "2026-07-20 14:30");
    }

    // ── send toggle and clear ────────────────────────────────────

    #[test]
    fn send_toggle_now_to_date() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_toggle();
        assert_eq!(dialog.send_choice, SendChoice::Date);
    }

    #[test]
    fn send_toggle_date_to_now_clears() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(chrono::Local::now().naive_local());
        dialog.send_edit = Some(SendAtEdit {
            value: chrono::Local::now().naive_local(),
            field: 0,
            typed: 0,
            typed_len: 0,
        });
        dialog.send_toggle();
        assert_eq!(dialog.send_choice, SendChoice::Now);
        assert!(dialog.send_at.is_none());
        assert!(dialog.send_edit.is_none());
    }

    #[test]
    fn clear_send_at() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(chrono::Local::now().naive_local());
        dialog.send_edit = Some(SendAtEdit {
            value: chrono::Local::now().naive_local(),
            field: 0,
            typed: 0,
            typed_len: 0,
        });
        dialog.clear_send_at();
        assert_eq!(dialog.send_choice, SendChoice::Now);
        assert!(dialog.send_at.is_none());
        assert!(dialog.send_edit.is_none());
    }

    // ── send_edit operations ─────────────────────────────────────

    #[test]
    fn send_edit_move_clamps_field() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit_move(10);
        let edit = dialog.send_edit.unwrap();
        assert_eq!(edit.field, 4); // clamped to minute
        dialog.send_edit_move(-100);
        let edit = dialog.send_edit.unwrap();
        assert_eq!(edit.field, 0); // clamped to year
    }

    #[test]
    fn send_edit_adjust_day() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        let before = dialog.send_edit.as_ref().unwrap().value;
        dialog.send_edit.as_mut().unwrap().field = 2; // day
        dialog.send_edit_adjust(1);
        let edit = dialog.send_edit.unwrap();
        let expected = (before + chrono::Duration::days(1)).day();
        assert_eq!(edit.value.day(), expected);
    }

    #[test]
    fn send_edit_type_digit_auto_advances_field() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 3; // hour (width 2)
        dialog.send_edit_type_digit(1);
        // After 1 digit (not yet full), typed=1, typed_len=1
        assert_eq!(dialog.send_edit.as_ref().unwrap().typed, 1);
        assert_eq!(dialog.send_edit.as_ref().unwrap().typed_len, 1);
        dialog.send_edit_type_digit(4);
        // After 2 digits (field full), auto-advances to minute, typed resets
        assert_eq!(dialog.send_edit.as_ref().unwrap().field, 4);
        assert_eq!(dialog.send_edit.as_ref().unwrap().typed, 0);
    }

    #[test]
    fn send_edit_confirm_past_rejects() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().value = chrono::NaiveDateTime::default(); // epoch
        assert!(!dialog.send_edit_confirm());
        assert!(dialog.send_error.is_some());
    }

    #[test]
    fn send_edit_cancel_no_prior_send_returns_now() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit_cancel();
        assert!(dialog.send_edit.is_none());
        assert_eq!(dialog.send_choice, SendChoice::Now);
    }

    #[test]
    fn send_edit_cancel_with_prior_send_stays_date() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(chrono::Local::now().naive_local());
        dialog.send_begin_edit();
        dialog.send_edit_cancel();
        assert!(dialog.send_edit.is_none());
        assert_eq!(dialog.send_choice, SendChoice::Date);
    }

    // ── is_send_at_focused ───────────────────────────────────────

    #[test]
    fn is_send_at_focused_true() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 0;
        assert!(dialog.is_send_at_focused());
    }

    #[test]
    fn is_send_at_focused_false_on_section() {
        let dialog = SimplePromptDialog::new();
        assert!(!dialog.is_send_at_focused());
    }

    // ── total_focusable ──────────────────────────────────────────

    #[test]
    fn total_focusable_empty_sections() {
        let mut dialog = SimplePromptDialog::new();
        dialog.enabled_sections.clear();
        assert_eq!(dialog.total_focusable(), 1); // just send control
    }

    #[test]
    fn total_focusable_with_sections() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("goal");
        dialog.add_section("context");
        // instruction_1 + goal_1 + context_1 = 3 sections + send = 4
        assert_eq!(dialog.total_focusable(), 4);
    }

    // ── section_entries and section_lines ─────────────────────────

    #[test]
    fn section_entries_filters_empty() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "  ".to_string());
        assert!(dialog.section_entries("instruction").is_empty());
    }

    #[test]
    fn section_entries_includes_non_empty() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "do something".to_string());
        assert_eq!(dialog.section_entries("instruction").len(), 1);
    }

    #[test]
    fn section_lines_splits_multiline() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "line1\nline2\nline3".to_string());
        let lines = dialog.section_lines("instruction");
        assert_eq!(lines.len(), 3);
    }

    // ── build_body ───────────────────────────────────────────────

    #[test]
    fn build_body_empty_dialog() {
        let dialog = SimplePromptDialog::new();
        let body = dialog.build_body();
        assert!(!body.contains("GOAL"));
        assert!(!body.contains("CONTEXT"));
        assert!(body.contains("INSTRUCTIONS"));
    }

    #[test]
    fn build_body_with_goal() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "do it".to_string());
        dialog.add_section_with_content("goal", "ship the feature".to_string());
        let body = dialog.build_body();
        assert!(body.contains("GOAL"));
        assert!(body.contains("ship the feature"));
    }

    // ── collect_tool_lines ───────────────────────────────────────

    #[test]
    fn collect_tool_lines_empty() {
        let dialog = SimplePromptDialog::new();
        assert!(dialog.collect_tool_lines().is_empty());
    }

    #[test]
    fn collect_tool_lines_with_content() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section_with_content(
            "tools",
            "skill:code-engineering\nskill:rust-idiomatic".to_string(),
        );
        let lines = dialog.collect_tool_lines();
        assert_eq!(lines.len(), 2);
    }

    // ── PromptBuilderSession round-trip ──────────────────────────

    #[test]
    fn prompt_builder_session_from_dialog_copies_all_fields() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "test".to_string());
        dialog.add_section_with_content("context", "ctx".to_string());
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(chrono::Local::now().naive_local());

        let session = PromptBuilderSession::from_dialog(&dialog);
        assert_eq!(
            session.sections.get("instruction_1").map(String::as_str),
            Some("test")
        );
        // Find the context section (it will have a generated name like "context_1" or "context_2")
        let ctx_val = session
            .sections
            .iter()
            .find(|(k, _)| k.starts_with("context"))
            .map(|(_, v)| v.as_str());
        assert_eq!(ctx_val, Some("ctx"));
        // send_at is persisted in PromptBuilderSession
        assert!(session.send_at.is_some());
    }

    // ── PersistedBuilderState round-trip ─────────────────────────

    #[test]
    fn persisted_builder_state_excludes_send_at() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "test".to_string());
        dialog.send_choice = SendChoice::Date;
        dialog.send_at = Some(chrono::Local::now().naive_local());

        let snapshot = PersistedBuilderState::from_dialog(&dialog);
        let json = serde_json::to_string(&snapshot).unwrap();
        let restored: PersistedBuilderState = serde_json::from_str(&json).unwrap();

        let mut target = SimplePromptDialog::new();
        restored.restore_into(&mut target);
        // send_at must NOT be restored from persisted state
        assert!(target.send_at.is_none());
    }

    // ── get_removable_sections ───────────────────────────────────

    #[test]
    fn get_removable_sections_single_instruction_not_removable() {
        let dialog = SimplePromptDialog::new();
        let removable = dialog.get_removable_sections();
        assert!(removable.is_empty());
    }

    #[test]
    fn get_removable_sections_multiple_instructions_all_removable() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("instruction");
        let removable = dialog.get_removable_sections();
        assert_eq!(removable.len(), 2);
    }

    #[test]
    fn get_removable_sections_non_instruction_always_removable() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("context");
        let removable = dialog.get_removable_sections();
        assert!(removable.iter().any(|(id, _)| id.starts_with("context")));
    }

    // ── remove_section protection ────────────────────────────────

    #[test]
    fn remove_section_last_instruction_protected() {
        let mut dialog = SimplePromptDialog::new();
        let id = dialog.enabled_sections[0].clone();
        dialog.remove_section(&id);
        // Should not have removed it
        assert_eq!(dialog.enabled_sections.len(), 1);
    }

    #[test]
    fn remove_section_non_instruction_removable() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("context");
        let ctx_id = dialog
            .enabled_sections
            .iter()
            .find(|id| id.starts_with("context"))
            .cloned()
            .unwrap();
        let len_before = dialog.enabled_sections.len();
        dialog.remove_section(&ctx_id);
        assert_eq!(dialog.enabled_sections.len(), len_before - 1);
    }

    // ── get_section_content / set_section_content ─────────────────

    #[test]
    fn get_section_content_missing_returns_empty() {
        let dialog = SimplePromptDialog::new();
        assert!(dialog.get_section_content("nonexistent").is_empty());
    }

    #[test]
    fn set_section_content_round_trip() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "hello".to_string());
        assert_eq!(dialog.get_section_content("instruction_1"), "hello");
    }

    // ── section_content_for_build with collapsed paste ────────────

    #[test]
    fn section_content_for_build_returns_collapsed_paste() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "[Pasted ~3 lines]".to_string());
        dialog.collapsed_pastes.insert(
            "instruction_1".to_string(),
            "real content\nmore lines\nfinal".to_string(),
        );
        assert_eq!(
            dialog.section_content_for_build("instruction_1"),
            Some("real content\nmore lines\nfinal")
        );
    }

    // ── should_collapse_paste ────────────────────────────────────

    #[test]
    fn should_collapse_paste_single_short_line() {
        assert!(!SimplePromptDialog::should_collapse_paste("hello"));
    }

    #[test]
    fn should_collapse_paste_multiline() {
        assert!(SimplePromptDialog::should_collapse_paste("line1\nline2"));
    }

    #[test]
    fn should_collapse_paste_long_line() {
        assert!(SimplePromptDialog::should_collapse_paste(&"a".repeat(201)));
    }

    #[test]
    fn should_collapse_paste_exactly_200_chars() {
        assert!(!SimplePromptDialog::should_collapse_paste(&"a".repeat(200)));
    }

    // ── expand_collapsed_paste ───────────────────────────────────

    #[test]
    fn expand_collapsed_paste_restores_content() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "[Pasted ~2 lines]".to_string());
        dialog
            .collapsed_pastes
            .insert("instruction_1".to_string(), "real\ncontent".to_string());
        dialog.expand_collapsed_paste("instruction_1");
        assert_eq!(dialog.get_section_content("instruction_1"), "real\ncontent");
        assert!(!dialog.has_collapsed_paste("instruction_1"));
    }

    #[test]
    fn expand_collapsed_paste_noop_without_paste() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "normal".to_string());
        dialog.expand_collapsed_paste("instruction_1");
        assert_eq!(dialog.get_section_content("instruction_1"), "normal");
    }

    // ── cursor_in_collapsed_placeholder ───────────────────────────

    #[test]
    fn cursor_in_collapsed_placeholder_no_paste() {
        let dialog = SimplePromptDialog::new();
        assert!(!dialog.cursor_in_collapsed_placeholder("instruction_1"));
    }

    // ── scroll_raw_preview ───────────────────────────────────────

    #[test]
    fn scroll_raw_preview_clamps() {
        let mut dialog = SimplePromptDialog::new();
        dialog.raw_preview = Some("line1\nline2\nline3".to_string());
        dialog.scroll_raw_preview(100);
        assert_eq!(dialog.raw_preview_scroll, 2); // max = 3 lines - 1 = 2
    }

    #[test]
    fn scroll_raw_preview_negative_clamps_to_zero() {
        let mut dialog = SimplePromptDialog::new();
        dialog.raw_preview = Some("line1\nline2".to_string());
        dialog.raw_preview_scroll = 0;
        dialog.scroll_raw_preview(-5);
        assert_eq!(dialog.raw_preview_scroll, 0);
    }

    // ── load_flat_text ───────────────────────────────────────────

    #[test]
    fn load_flat_text_resets_counters() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("goal");
        dialog.add_section("context");
        dialog.load_flat_text("recovered");
        assert_eq!(dialog.enabled_sections.len(), 1);
        assert_eq!(dialog.get_section_content("instruction_1"), "recovered");
    }

    // ── insert_collapsed_paste_at_cursor short text ──────────────

    #[test]
    fn insert_collapsed_paste_short_text_not_collapsed() {
        let mut dialog = SimplePromptDialog::new();
        dialog.insert_collapsed_paste_at_cursor("instruction_1", "short", 80);
        assert_eq!(dialog.get_section_content("instruction_1"), "short");
        assert!(!dialog.has_collapsed_paste("instruction_1"));
    }

    // ── backspace_collapsed_paste without paste ───────────────────

    #[test]
    fn backspace_collapsed_paste_noop_without_paste() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "normal text".to_string());
        dialog.backspace_collapsed_paste("instruction_1", 80);
        assert_eq!(dialog.get_section_content("instruction_1"), "normal text");
    }

    // ── add_resource_reference ───────────────────────────────────

    #[test]
    fn add_resource_reference_no_existing_section() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_resource_reference("/path/to/file");
        assert!(dialog
            .enabled_sections
            .iter()
            .any(|id| id.starts_with("resources")));
        assert_eq!(
            dialog.get_section_content(&dialog.resources_section_id().unwrap()),
            "/path/to/file"
        );
    }

    #[test]
    fn add_resource_reference_appends_to_existing() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section_with_content("resources", "existing".to_string());
        dialog.add_resource_reference("/new/path");
        let content = dialog.get_section_content(&dialog.resources_section_id().unwrap());
        assert!(content.contains("existing"));
        assert!(content.contains("/new/path"));
    }

    // ── resources_section_id ─────────────────────────────────────

    #[test]
    fn resources_section_id_none_when_empty() {
        let dialog = SimplePromptDialog::new();
        assert!(dialog.resources_section_id().is_none());
    }

    #[test]
    fn resources_section_id_found() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("resources");
        assert!(dialog.resources_section_id().is_some());
    }

    // ── display name ─────────────────────────────────────────────

    #[test]
    fn section_display_name_with_number() {
        let name = SimplePromptDialog::section_display_name("instruction_1");
        assert!(name.contains("Instruction"));
        assert!(name.contains("1"));
    }

    #[test]
    fn section_display_name_without_number() {
        let name = SimplePromptDialog::section_display_name("tools");
        assert!(name.contains("Tools"));
    }

    // ── format_file_resource ────────────────────────────────────

    #[test]
    fn format_file_resource_shows_path_and_kind() {
        let result = SimplePromptDialog::format_file_resource(Path::new("/tmp/test.rs"));
        assert!(result.contains("path: /tmp/test.rs"));
        assert!(result.contains("kind: file"));
    }

    // ── format_project_block ────────────────────────────────────

    #[test]
    fn format_project_block_required_fields() {
        use crate::domain::project::Project;
        let project = Project {
            hash: "abc123".to_string(),
            name: "test-project".to_string(),
            path: "/tmp/project".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 0,
        };
        let result = SimplePromptDialog::format_project_block(&project);
        assert!(result.contains("name: test-project"));
        assert!(result.contains("workdir_hash: abc123"));
        assert!(result.contains("path: /tmp/project"));
        assert!(!result.contains("description:"));
        assert!(!result.contains("tags:"));
        assert!(!result.contains("indexed_at:"));
    }

    #[test]
    fn format_project_block_with_description() {
        use crate::domain::project::Project;
        let project = Project {
            hash: "def456".to_string(),
            name: "proj".to_string(),
            path: "/p".to_string(),
            description: Some("a project".to_string()),
            tags: None,
            indexed_at: None,
            created_at: 0,
        };
        let result = SimplePromptDialog::format_project_block(&project);
        assert!(result.contains("description: a project"));
    }

    #[test]
    fn format_project_block_with_tags() {
        use crate::domain::project::Project;
        let project = Project {
            hash: "ghi".to_string(),
            name: "p".to_string(),
            path: "/p".to_string(),
            description: None,
            tags: Some("rust,tui".to_string()),
            indexed_at: None,
            created_at: 0,
        };
        let result = SimplePromptDialog::format_project_block(&project);
        assert!(result.contains("tags: rust,tui"));
    }

    #[test]
    fn format_project_block_with_indexed_at() {
        use crate::domain::project::Project;
        let project = Project {
            hash: "jkl".to_string(),
            name: "p".to_string(),
            path: "/p".to_string(),
            description: None,
            tags: None,
            indexed_at: Some(1700000000),
            created_at: 0,
        };
        let result = SimplePromptDialog::format_project_block(&project);
        assert!(result.contains("indexed_at:"));
    }

    // ── is_locked / lock_section ────────────────────────────────

    #[test]
    fn is_locked_false_for_new_section() {
        let dialog = SimplePromptDialog::new();
        assert!(!dialog.is_locked("instruction_1"));
    }

    #[test]
    fn is_locked_true_after_lock() {
        let mut dialog = SimplePromptDialog::new();
        dialog.lock_section("instruction_1");
        assert!(dialog.is_locked("instruction_1"));
    }

    #[test]
    fn is_locked_false_for_other_section() {
        let mut dialog = SimplePromptDialog::new();
        dialog.lock_section("instruction_1");
        assert!(!dialog.is_locked("context_1"));
    }

    // ── set_tools_section_skill ─────────────────────────────────

    #[test]
    fn set_tools_section_skill_sets_content() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("tools");
        let tools_id = dialog
            .enabled_sections
            .iter()
            .find(|id| id.starts_with("tools"))
            .cloned()
            .unwrap();
        dialog.set_tools_section_skill(&tools_id, "skill:code-engineering");
        assert_eq!(
            dialog.get_section_content(&tools_id),
            "skill:code-engineering"
        );
    }

    #[test]
    fn set_tools_section_skill_replaces_existing() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section_with_content("tools", "old-skill".to_string());
        let tools_id = dialog
            .enabled_sections
            .iter()
            .find(|id| id.starts_with("tools"))
            .cloned()
            .unwrap();
        dialog.set_tools_section_skill(&tools_id, "new-skill");
        assert_eq!(dialog.get_section_content(&tools_id), "new-skill");
    }

    // ── generate_section_id ─────────────────────────────────────

    #[test]
    fn generate_section_id_increments_counter() {
        let mut dialog = SimplePromptDialog::new();
        let id1 = dialog.generate_section_id("goal");
        let id2 = dialog.generate_section_id("goal");
        let id3 = dialog.generate_section_id("goal");
        assert_eq!(id1, "goal_1");
        assert_eq!(id2, "goal_2");
        assert_eq!(id3, "goal_3");
    }

    #[test]
    fn generate_section_id_independent_per_type() {
        let mut dialog = SimplePromptDialog::new();
        let g = dialog.generate_section_id("goal");
        let c = dialog.generate_section_id("context");
        assert_eq!(g, "goal_1");
        // context counter starts at 2 from new()
        assert_eq!(c, "context_2");
    }

    // ── instruction_count ───────────────────────────────────────

    #[test]
    fn instruction_count_single() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.instruction_count(), 1);
    }

    #[test]
    fn instruction_count_after_add() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("instruction");
        assert_eq!(dialog.instruction_count(), 2);
    }

    #[test]
    fn instruction_count_after_remove() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("instruction");
        assert_eq!(dialog.instruction_count(), 2);
        let extra = dialog
            .enabled_sections
            .iter()
            .find(|id| id.starts_with("instruction") && *id != "instruction_1")
            .cloned()
            .unwrap();
        dialog.remove_section(&extra);
        assert_eq!(dialog.instruction_count(), 1);
    }

    // ── cursor / scroll edge cases ──────────────────────────────

    #[test]
    fn cursor_default_for_missing_section() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.cursor("nonexistent"), 0);
    }

    #[test]
    fn scroll_default_for_missing_section() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.scroll("nonexistent"), 0);
    }

    #[test]
    fn cursor_returns_stored_value() {
        let mut dialog = SimplePromptDialog::new();
        dialog
            .section_cursors
            .insert("instruction_1".to_string(), 5);
        assert_eq!(dialog.cursor("instruction_1"), 5);
    }

    #[test]
    fn scroll_returns_stored_value() {
        let mut dialog = SimplePromptDialog::new();
        dialog
            .section_scrolls
            .insert("instruction_1".to_string(), 3);
        assert_eq!(dialog.scroll("instruction_1"), 3);
    }

    // ── is_file_reference more edge cases ───────────────────────

    #[test]
    fn is_file_reference_with_path_separator() {
        assert!(SimplePromptDialog::is_file_reference("@src/main.rs"));
        assert!(SimplePromptDialog::is_file_reference("@a/b/c"));
    }

    #[test]
    fn is_file_reference_with_underscore() {
        assert!(SimplePromptDialog::is_file_reference("@my_file"));
    }

    #[test]
    fn is_file_reference_with_hyphen() {
        assert!(SimplePromptDialog::is_file_reference("@my-file"));
    }

    #[test]
    fn is_file_reference_with_dot() {
        assert!(SimplePromptDialog::is_file_reference("@file.txt"));
        assert!(SimplePromptDialog::is_file_reference("@.hidden"));
    }

    // ── next_file_reference edge cases ──────────────────────────

    #[test]
    fn next_file_reference_at_end_of_string() {
        let text = "hello @";
        assert!(SimplePromptDialog::next_file_reference(text, 0).is_some());
    }

    #[test]
    fn next_file_reference_at_only_char() {
        let text = "@";
        let result = SimplePromptDialog::next_file_reference(text, 0);
        assert!(result.is_some());
    }

    // ── visual_line_count more edge cases ───────────────────────

    #[test]
    fn visual_line_count_mixed_newlines_and_wrap() {
        // "ab\ncdefghij" with width 5 → "ab" (1 line) + newline → "cdef" + "ghij" = 3 lines
        assert_eq!(SimplePromptDialog::visual_line_count("ab\ncdefghij", 5), 3);
    }

    #[test]
    fn visual_line_count_tab_at_end_of_line() {
        // Tab at col 2 with width 5: tab takes 2 cols (4 - 2%4 = 2), total col=4 ≤ 5 → 1 line
        assert_eq!(SimplePromptDialog::visual_line_count("ab\t", 5), 1);
    }

    #[test]
    fn visual_line_count_multiple_tabs() {
        assert_eq!(SimplePromptDialog::visual_line_count("\t\t", 10), 1);
    }

    // ── get_available_sections ──────────────────────────────────

    #[test]
    fn get_available_sections_has_core_types() {
        let sections = SimplePromptDialog::get_available_sections();
        let names: Vec<&str> = sections.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"instruction"));
        assert!(names.contains(&"goal"));
        assert!(names.contains(&"context"));
        assert!(names.contains(&"resources"));
        assert!(names.contains(&"constraints"));
        assert!(names.contains(&"tools"));
        assert!(names.contains(&"preset"));
    }

    // ── focused_section_name edge cases ─────────────────────────

    #[test]
    fn focused_section_name_send_control_returns_none() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 0;
        assert!(dialog.focused_section_name().is_none());
    }

    #[test]
    fn focused_section_name_first_section() {
        let dialog = SimplePromptDialog::new();
        assert_eq!(dialog.focused_section_name(), Some("instruction_1"));
    }

    #[test]
    fn focused_section_name_out_of_bounds_returns_none() {
        let mut dialog = SimplePromptDialog::new();
        dialog.focused_section = 99;
        assert!(dialog.focused_section_name().is_none());
    }

    // ── add_resource_reference: existing content is empty ───────

    #[test]
    fn add_resource_reference_empty_existing_content() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section_with_content("resources", String::new());
        dialog.add_resource_reference("/new/path");
        let rid = dialog.resources_section_id().unwrap();
        assert_eq!(dialog.get_section_content(&rid), "/new/path");
    }

    // ── strip_resources_section edge cases ───────────────────────

    #[test]
    fn strip_resources_section_empty_string() {
        assert_eq!(strip_resources_section(""), "");
    }

    #[test]
    fn strip_resources_section_only_header_no_closing() {
        let input = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n";
        assert_eq!(strip_resources_section(input), input);
    }

    // ── remove_section: second instruction removable ────────────

    #[test]
    fn remove_section_second_instruction_removable() {
        let mut dialog = SimplePromptDialog::new();
        dialog.add_section("instruction");
        let extra_id = dialog
            .enabled_sections
            .iter()
            .find(|id| id.starts_with("instruction") && *id != "instruction_1")
            .cloned()
            .unwrap();
        let len_before = dialog.enabled_sections.len();
        dialog.remove_section(&extra_id);
        assert_eq!(dialog.enabled_sections.len(), len_before - 1);
    }

    // ── set_tab no-op when already active ───────────────────────

    #[test]
    fn set_tab_same_tab_is_noop() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_tab(PromptTab::Normal);
        assert_eq!(dialog.active_tab, PromptTab::Normal);
        assert_eq!(dialog.focused_section, 1);
    }

    #[test]
    fn set_tab_raw_to_normal_focuses_first_section() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_tab(PromptTab::Raw);
        assert_eq!(dialog.active_tab, PromptTab::Raw);
        dialog.set_tab(PromptTab::Normal);
        assert_eq!(dialog.active_tab, PromptTab::Normal);
        assert_eq!(dialog.focused_section, 1);
    }

    // ── send_edit_type_digit: restart on full field ─────────────

    #[test]
    fn send_edit_type_digit_restarts_full_field() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 3; // hour (width 2)
        dialog.send_edit_type_digit(1);
        dialog.send_edit_type_digit(4);
        // Now on minute, auto-advanced
        assert_eq!(dialog.send_edit.as_ref().unwrap().field, 4);
        // Type a digit that fills the field
        dialog.send_edit_type_digit(5);
        dialog.send_edit_type_digit(9);
        // Minute full, auto-advanced would go to field 5 (clamped to 4)
        // But if we type again on a full field, it restarts
        dialog.send_edit_type_digit(3);
        let edit = dialog.send_edit.as_ref().unwrap();
        assert_eq!(edit.typed, 3);
        assert_eq!(edit.typed_len, 1);
    }

    // ── send_edit_adjust: all fields ────────────────────────────

    #[test]
    fn send_edit_adjust_year() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 0;
        dialog.send_edit_adjust(2);
        let year = dialog.send_edit.unwrap().value.year();
        let now_year = chrono::Local::now().naive_local().year();
        assert_eq!(year, now_year + 2);
    }

    #[test]
    fn send_edit_adjust_month() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 1;
        dialog.send_edit_adjust(1);
        let month = dialog.send_edit.unwrap().value.month();
        let now_month = chrono::Local::now().naive_local().month();
        assert_eq!(month, (now_month % 12) + 1);
    }

    #[test]
    fn send_edit_adjust_hour() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 3;
        dialog.send_edit_adjust(1);
        let hour = dialog.send_edit.unwrap().value.hour();
        let now_hour = chrono::Local::now().naive_local().hour();
        assert_eq!(hour, (now_hour + 1) % 24);
    }

    #[test]
    fn send_edit_adjust_minute() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_begin_edit();
        dialog.send_edit.as_mut().unwrap().field = 4;
        dialog.send_edit_adjust(5);
        let minute = dialog.send_edit.unwrap().value.minute();
        let now_minute = chrono::Local::now().naive_local().minute();
        assert_eq!(minute, (now_minute + 5) % 60);
    }

    // ── send_edit_adjust_no_edit_is_noop ────────────────────────

    #[test]
    fn send_edit_adjust_no_edit_is_noop() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_edit_adjust(5);
        assert!(dialog.send_edit.is_none());
    }

    #[test]
    fn send_edit_type_digit_no_edit_is_noop() {
        let mut dialog = SimplePromptDialog::new();
        dialog.send_edit_type_digit(5);
        assert!(dialog.send_edit.is_none());
    }

    // ── visual_positions ────────────────────────────────────────

    #[test]
    fn visual_positions_empty_string() {
        let positions = SimplePromptDialog::visual_positions("", 10);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0], (0, 0));
    }

    #[test]
    fn visual_positions_single_char() {
        let positions = SimplePromptDialog::visual_positions("a", 10);
        assert_eq!(positions.len(), 2);
        assert_eq!(positions[0], (0, 0));
        assert_eq!(positions[1], (0, 1));
    }

    #[test]
    fn visual_positions_wrap_at_width() {
        let positions = SimplePromptDialog::visual_positions("abcde", 3);
        assert_eq!(positions.len(), 6);
        // 'a' col 0→1, 'b' col 1→2, 'c' col 2→3, 'd' col 3 wraps to (1,1), 'e' col 1→2
        assert_eq!(positions[4], (1, 1));
        assert_eq!(positions[5], (1, 2));
    }

    // ── build_xml_block ─────────────────────────────────────────

    #[test]
    fn build_xml_block_empty_sections() {
        let mut result = String::new();
        build_xml_block(
            &mut result,
            &[],
            |_| true,
            |_| Some("content"),
            "# Header\n",
            "outer",
            "item",
        );
        assert!(result.is_empty());
    }

    #[test]
    fn build_xml_block_no_matching_sections() {
        let mut result = String::new();
        let sections = vec!["goal_1".to_string()];
        build_xml_block(
            &mut result,
            &sections,
            |id| id.starts_with("context"),
            |_| Some("content"),
            "# Header\n",
            "outer",
            "item",
        );
        assert!(result.is_empty());
    }

    #[test]
    fn build_xml_block_matching_empty_content_skipped() {
        let mut result = String::new();
        let sections = vec!["context_1".to_string()];
        build_xml_block(
            &mut result,
            &sections,
            |id| id.starts_with("context"),
            |_| Some("  "),
            "# Header\n",
            "outer",
            "item",
        );
        assert!(result.is_empty());
    }

    // ── get_file_reference_with_styling ─────────────────────────

    #[test]
    fn get_file_reference_with_styling_no_refs() {
        let dialog = SimplePromptDialog::new();
        let result = dialog.get_file_reference_with_styling("hello world", Color::Blue);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "hello world");
        assert!(result[0].1.is_none());
    }

    #[test]
    fn get_file_reference_with_styling_with_ref() {
        let dialog = SimplePromptDialog::new();
        let result = dialog.get_file_reference_with_styling("look @src/lib.rs here", Color::Blue);
        // "look " + "@src/lib.rs" + " here"
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, "look ");
        assert!(result[0].1.is_none());
        assert_eq!(result[1].0, "@src/lib.rs");
        assert_eq!(result[1].1, Some(Color::Blue));
        assert_eq!(result[2].0, " here");
        assert!(result[2].1.is_none());
    }

    #[test]
    fn get_file_reference_with_styling_invalid_ref() {
        let dialog = SimplePromptDialog::new();
        let result = dialog.get_file_reference_with_styling("@ invalid", Color::Blue);
        // "@" is a valid ref start but "invalid" is part of " @ invalid"
        // Actually: next_file_reference finds @ at position 0, ref = "@", which has
        // len 1 and starts with @, so is_file_reference returns false.
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "@");
        assert!(result[0].1.is_none());
    }

    // ── insert_at_completion ────────────────────────────────────

    #[test]
    fn insert_at_completion_no_picker_is_noop() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "hello @".to_string());
        dialog.at_picker = None;
        dialog.insert_at_completion("instruction_1", "file.rs", "/abs/file.rs", 80);
        // Content unchanged
        assert_eq!(dialog.get_section_content("instruction_1"), "hello @");
    }

    // ── replace_char_range ──────────────────────────────────────

    #[test]
    fn replace_char_range_basic() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "hello world".to_string());
        dialog.replace_char_range("instruction_1", 5, 6, "@", 80);
        assert_eq!(dialog.get_section_content("instruction_1"), "hello@world");
    }

    #[test]
    fn replace_char_range_clamps_end() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "abc".to_string());
        dialog.replace_char_range("instruction_1", 1, 100, "X", 80);
        assert_eq!(dialog.get_section_content("instruction_1"), "aX");
    }

    #[test]
    fn replace_char_range_empty_replacement() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "abc".to_string());
        dialog.replace_char_range("instruction_1", 1, 2, "", 80);
        assert_eq!(dialog.get_section_content("instruction_1"), "ac");
    }

    // ── insert_section: multiple types ──────────────────────────

    #[test]
    fn insert_section_adds_to_enabled_and_focuses() {
        let mut dialog = SimplePromptDialog::new();
        let id = dialog.insert_section("constraint", "be careful".to_string());
        assert!(id.starts_with("constraint"));
        assert!(dialog.enabled_sections.contains(&id));
        assert_eq!(dialog.get_section_content(&id), "be careful");
        // Focus should be on the new section
        assert_eq!(dialog.focused_section, dialog.enabled_sections.len());
    }

    // ── resolve_resource_entry: URL ─────────────────────────────

    #[test]
    fn resolve_resource_entry_url() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();
        let result = SimplePromptDialog::resolve_resource_entry(&db, "https://example.com/doc");
        assert!(result.contains("https://example.com/doc"));
        assert!(result.contains("kind: url"));
    }

    #[test]
    fn resolve_resource_entry_raw_text() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("canopy.db");
        let db = Database::new(&db_path).unwrap();
        let result = SimplePromptDialog::resolve_resource_entry(&db, "just some text");
        assert!(result.contains("kind: raw"));
        assert!(result.contains("just some text"));
    }

    // ── section_content_for_build: no paste returns section ──────

    #[test]
    fn section_content_for_build_no_paste() {
        let mut dialog = SimplePromptDialog::new();
        dialog.set_section_content("instruction_1", "real".to_string());
        assert_eq!(
            dialog.section_content_for_build("instruction_1"),
            Some("real")
        );
    }

    #[test]
    fn section_content_for_build_missing_section() {
        let dialog = SimplePromptDialog::new();
        assert!(dialog.section_content_for_build("nonexistent").is_none());
    }
}

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
