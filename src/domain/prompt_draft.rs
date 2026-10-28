//! Prompt Builder text assembly, shared by the TUI dialog and the session hub.
//!
//! The TUI dialog used to own this: its sections, preset, skills and project
//! choices were rendered to the final prompt text by
//! `tui::app::dialog::prompt`. The remote web needs the same text without
//! reimplementing that assembly, so it lives here now as a pure function over
//! a serialisable [`PromptDraft`]:
//!
//! - the dialog converts its state with `to_prompt_draft()` and calls
//!   [`build_prompt_text`];
//! - the hub answers `compose` and draft `prompt` frames with it (see
//!   `sessions::catalog`).
//!
//! Byte-compatibility with the old dialog output is pinned by
//! `prompt_draft/golden_two_sections.txt`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::project::Project;
use super::prompts::render_preset;

/// One free-form builder section: `title` plays the role the dialog's
/// section id played (`goal`, `goal_1`, `instruction_2`, `tools`, …) and
/// decides which output group the content renders into.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftSection {
    pub title: String,
    pub content: String,
}

/// The remote's Prompt Builder state: free-form sections plus the named
/// preset, skills and project choices the hub resolves through a
/// [`PromptCatalog`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptDraft {
    #[serde(default)]
    pub sections: Vec<DraftSection>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub project: Option<String>,
}

/// A named preset from `~/.canopy/prompts/*.md`: file-stem name, first
/// non-empty line as the picker preview, and the full raw content.
#[derive(Debug, Clone, Default)]
pub struct PresetEntry {
    pub name: String,
    pub preview: String,
    pub content: String,
}

/// A skill the builder can reference: the bare `name` and the `label` as
/// stored in tools sections (`skill:…` for directory skills,
/// `canopy:…` for store skills).
#[derive(Debug, Clone, Default)]
pub struct SkillEntry {
    pub name: String,
    pub label: String,
}

/// A registered project: registry identity plus the pre-rendered
/// `block` ([`format_project_block`]) emitted into PROJECT CONTEXT.
#[derive(Debug, Clone, Default)]
pub struct ProjectEntry {
    pub hash: String,
    pub name: String,
    pub path: String,
    pub block: String,
}

/// Everything a draft's named choices resolve against. Loaded by
/// `prompt_catalog::PromptCatalog::load` from the same sources the TUI
/// pickers read.
#[derive(Debug, Clone, Default)]
pub struct PromptCatalog {
    pub presets: Vec<PresetEntry>,
    pub skills: Vec<SkillEntry>,
    pub projects: Vec<ProjectEntry>,
}

impl PromptCatalog {
    /// Find a preset by its file-stem name.
    pub fn preset(&self, name: &str) -> Option<&PresetEntry> {
        self.presets.iter().find(|entry| entry.name == name)
    }

    /// Find a skill by the label stored in tools sections, falling back to
    /// the bare name (first in catalog order wins — project skills shadow
    /// global ones, which shadow store skills, mirroring the picker).
    pub fn skill(&self, choice: &str) -> Option<&SkillEntry> {
        self.skills
            .iter()
            .find(|entry| entry.label == choice)
            .or_else(|| self.skills.iter().find(|entry| entry.name == choice))
    }

    /// Find a project by hash, name or path, mirroring the dialog's
    /// hash-then-path reference lookup (plus the name the pickers show).
    pub fn project(&self, choice: &str) -> Option<&ProjectEntry> {
        self.projects
            .iter()
            .find(|entry| entry.hash == choice || entry.name == choice || entry.path == choice)
    }
}

/// A draft choice that could not be built: every variant names the choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptDraftError {
    UnknownPreset(String),
    UnknownSkill(String),
    UnknownProject(String),
    UnrenderablePreset { preset: String, binding: String },
}

impl std::fmt::Display for PromptDraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PromptDraftError::UnknownPreset(name) => write!(f, "unknown preset '{name}'"),
            PromptDraftError::UnknownSkill(choice) => write!(f, "unknown skill '{choice}'"),
            PromptDraftError::UnknownProject(choice) => {
                write!(f, "unknown project '{choice}'")
            }
            PromptDraftError::UnrenderablePreset { preset, binding } => write!(
                f,
                "preset '{preset}' requires '{{{{{binding}}}}}', which was not supplied"
            ),
        }
    }
}

impl std::error::Error for PromptDraftError {}

/// The resolved side of a build: the catalog for named choices plus the
/// dialog-resolved blocks the hub has no equivalent of (a hub build leaves
/// them at their defaults).
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub catalog: PromptCatalog,
    /// Pre-resolved PROJECT CONTEXT blocks (the dialog's
    /// `resolve_project_contexts`); the draft's own project appends after.
    pub project_blocks: Vec<String>,
    /// Resolved resource items replacing the raw resources section.
    pub resources: Vec<String>,
    /// The invisible per-turn system content (`None` = omit).
    pub turn_context: Option<String>,
    /// Whether this send carries the one-time session-start protocol block.
    pub protocol_included: bool,
}

/// Whether a section id (or draft title) names a section `kind`: exact or
/// `kind_` + instance suffix.
pub fn section_matches(section_id: &str, kind: &str) -> bool {
    section_id == kind || section_id.starts_with(&format!("{kind}_"))
}

/// The trimmed non-empty contents of every section of one `kind`, in order.
fn section_contents<'a>(sections: &'a [DraftSection], kind: &str) -> Vec<&'a str> {
    sections
        .iter()
        .filter(|section| section_matches(&section.title, kind))
        .map(|section| section.content.trim())
        .filter(|content| !content.is_empty())
        .collect()
}

/// The trimmed non-empty lines of every tools section, in order — what the
/// dialog's `collect_tool_lines` produced for the same state.
pub fn tool_lines(sections: &[DraftSection]) -> Vec<String> {
    section_contents(sections, "tools")
        .into_iter()
        .flat_map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Emit one wrapped section — header, outer tag, one indented `item_tag`
/// block per item, closing tag — or nothing for an empty item list. The one
/// emission core behind [`build_xml_block`], [`format_indexed_xml_section`]
/// and the draft body groups, so the three can never drift apart.
fn append_items(
    result: &mut String,
    items: &[&str],
    header: &str,
    outer_tag: &str,
    item_tag: &str,
) {
    if items.is_empty() {
        return;
    }
    result.push_str(header);
    result.push_str(&format!("<{outer_tag}>\n"));
    for item in items {
        push_xml_item(result, item_tag, item);
    }
    result.push_str(&format!("</{outer_tag}>\n\n"));
}

fn append_section_group(
    result: &mut String,
    sections: &[DraftSection],
    kind: &str,
    header: &str,
    outer_tag: &str,
    item_tag: &str,
) {
    append_items(
        result,
        &section_contents(sections, kind),
        header,
        outer_tag,
        item_tag,
    );
}

/// The instruction group: section instructions in order, then the preset's
/// rendered content last (as the picker confirm appends it). Always emitted,
/// even empty — the dialog's empty builder still sends the block.
fn append_instruction_group(
    result: &mut String,
    draft: &PromptDraft,
    catalog: &PromptCatalog,
) -> Result<(), PromptDraftError> {
    result.push_str("# [INSTRUCTIONS]: Execution Logic\n");
    result.push_str("<instruction_set>\n");
    for content in section_contents(&draft.sections, "instruction") {
        push_xml_item(result, "instruction", content);
    }
    if let Some(name) = draft.preset.as_deref() {
        let entry = catalog
            .preset(name)
            .ok_or_else(|| PromptDraftError::UnknownPreset(name.to_string()))?;
        let rendered =
            render_preset(&entry.name, &entry.content, &HashMap::new()).map_err(|missing| {
                PromptDraftError::UnrenderablePreset {
                    preset: missing.preset,
                    binding: missing.binding,
                }
            })?;
        if !rendered.trim().is_empty() {
            push_xml_item(result, "instruction", rendered.trim());
        }
    }
    result.push_str("</instruction_set>\n\n");
    Ok(())
}

/// The tools group: tools-section lines first, then the draft's skills
/// resolved to their labels. Omitted when there are no lines at all.
fn append_tools_group(
    result: &mut String,
    draft: &PromptDraft,
    catalog: &PromptCatalog,
) -> Result<(), PromptDraftError> {
    let mut lines = tool_lines(&draft.sections);
    for choice in &draft.skills {
        let entry = catalog
            .skill(choice)
            .ok_or_else(|| PromptDraftError::UnknownSkill(choice.clone()))?;
        lines.push(entry.label.clone());
    }
    if lines.is_empty() {
        return Ok(());
    }
    result.push_str("# [TOOLS]: Skills & Capabilities\n");
    result.push_str("<tools>\n");
    for line in &lines {
        append_tool_skill(result, line);
    }
    result.push_str("</tools>\n\n");
    Ok(())
}

/// Build the prompt body (every section except the system block): the old
/// dialog group order — goal, context, instructions (+ preset last),
/// resources, constraints, tools (+ skills).
pub fn build_prompt_body(
    draft: &PromptDraft,
    catalog: &PromptCatalog,
) -> Result<String, PromptDraftError> {
    let mut result = String::new();
    append_section_group(
        &mut result,
        &draft.sections,
        "goal",
        "# [GOAL]: Desired Outcome\n",
        "goal",
        "goal_item",
    );
    append_section_group(
        &mut result,
        &draft.sections,
        "context",
        "# [CONTEXT]: Project Background\n",
        "context",
        "context",
    );
    append_instruction_group(&mut result, draft, catalog)?;
    append_section_group(
        &mut result,
        &draft.sections,
        "resources",
        "# [RESOURCES]: Knowledge Base & Data\n",
        "resources",
        "resource",
    );
    append_section_group(
        &mut result,
        &draft.sections,
        "constraints",
        "# [CONSTRAINTS]: Behavioral Boundaries\n",
        "constraints",
        "constraint",
    );
    append_tools_group(&mut result, draft, catalog)?;
    Ok(result)
}

/// The assembled system-block content: the protocol once at the very top
/// (when it rides along), then the per-turn context. Mirrors the dialog's
/// `system_text` plus its `<system>` wrap.
fn system_block(turn_context: Option<&str>, with_protocol: bool) -> String {
    let turn = turn_context.unwrap_or_default();
    let system = if !with_protocol {
        turn.to_string()
    } else {
        let block = crate::domain::sandbox::canopy_protocol_block();
        if turn.is_empty() {
            block.to_string()
        } else {
            format!("{block}\n\n{turn}")
        }
    };
    if system.is_empty() {
        String::new()
    } else {
        format!("<system>\n{system}\n</system>\n\n")
    }
}

/// Build the exact text the dialog sends for these choices: the system block
/// first, PROJECT CONTEXT (resolved blocks plus the draft's own project)
/// leading the body, and resolved resources replacing the raw section.
pub fn build_prompt_text(
    draft: &PromptDraft,
    ctx: &PromptContext,
) -> Result<String, PromptDraftError> {
    let with_protocol = ctx.protocol_included
        || draft
            .sections
            .iter()
            .any(|section| section_matches(&section.title, "canopy_protocol"));
    let system = system_block(ctx.turn_context.as_deref(), with_protocol);
    let mut body = build_prompt_body(draft, &ctx.catalog)?;

    let mut blocks = ctx.project_blocks.clone();
    if let Some(choice) = draft.project.as_deref() {
        let entry = ctx
            .catalog
            .project(choice)
            .ok_or_else(|| PromptDraftError::UnknownProject(choice.to_string()))?;
        blocks.push(entry.block.clone());
    }
    if let Some(section) = format_indexed_xml_section(
        "# [PROJECT CONTEXT]: Registered Project Metadata\n",
        "project_context",
        "project",
        &blocks,
    ) {
        body = format!("{section}{body}");
    }

    // Resolved resources replace the raw resources section and close the body.
    if let Some(section) = format_indexed_xml_section(
        "# [RESOURCES]: Knowledge Base & Data\n",
        "resources",
        "resource",
        &ctx.resources,
    ) {
        body = strip_resources_section(&body);
        body.push_str(&section);
    }

    // System block stays first so its protocol is read before the task.
    Ok(format!("{system}{body}"))
}

/// Render one project registry row as the `project` item content of PROJECT
/// CONTEXT. Moved verbatim from the dialog's `format_project_block`.
pub fn format_project_block(project: &Project) -> String {
    let mut lines = vec![
        format!("name: {}", project.name),
        format!("workdir_hash: {}", project.hash),
        format!("path: {}", project.path),
    ];
    if let Some(description) = project.description.as_deref() {
        lines.push(format!("description: {description}"));
    }
    if let Some(tags) = project.tags.as_deref() {
        lines.push(format!("tags: {tags}"));
    }
    if let Some(indexed_at) = project.indexed_at {
        lines.push(format!("indexed_at: {indexed_at}"));
    }
    lines.join("\n")
}

/// Render one tool line as a `<skill>` item; `canopy:` lines become a
/// `skill_get` fetch instruction. Moved verbatim from the dialog.
pub fn append_tool_skill(result: &mut String, tool_line: &str) {
    if let Some(name) = tool_line.strip_prefix("canopy:") {
        result.push_str("  <skill>\n");
        result.push_str(&format!(
            "    canopy skill {name} — load it with skill_get(name=\"{name}\")\n"
        ));
        result.push_str("  </skill>\n\n");
    } else {
        result.push_str("  <skill>\n");
        result.push_str(&format!("    {tool_line}\n"));
        result.push_str("  </skill>\n\n");
    }
}

/// Remove the raw resources section from a built body so resolved resources
/// can take its place. Moved verbatim from the dialog.
pub fn strip_resources_section(prompt: &str) -> String {
    let header = "# [RESOURCES]: Knowledge Base & Data\n<resources>\n";
    let Some(start) = prompt.find(header) else {
        return prompt.to_string();
    };
    let Some(end_rel) = prompt[start..].find("</resources>\n\n") else {
        return prompt.to_string();
    };

    let end = start + end_rel + "</resources>\n\n".len();
    let mut stripped = String::with_capacity(prompt.len().saturating_sub(end - start));
    stripped.push_str(&prompt[..start]);
    stripped.push_str(&prompt[end..]);
    stripped
}

/// Append one indented `<tag>` item block. Moved verbatim from the dialog.
pub fn push_xml_item(result: &mut String, tag: &str, content: &str) {
    result.push_str(&format!("  <{tag}>\n"));
    for line in content.lines() {
        result.push_str(&format!("    {line}\n"));
    }
    result.push_str(&format!("  </{tag}>\n\n"));
}

/// Render a standalone section (PROJECT CONTEXT, resolved resources) from
/// pre-built items, or `None` when there is nothing to show. Moved from the
/// dialog; the emission runs through [`append_items`].
pub fn format_indexed_xml_section(
    header: &str,
    outer_tag: &str,
    item_tag: &str,
    items: &[String],
) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut result = String::new();
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    append_items(&mut result, &refs, header, outer_tag, item_tag);
    Some(result)
}

/// Build a wrapped XML section (header + outer tag + items) from matching
/// section IDs. Moved from the dialog with its signature intact; the
/// emission runs through [`append_items`]. Test-only now: production builds
/// through [`build_prompt_body`], and this stays so the unchanged dialog
/// tests still resolve the helper.
#[cfg(test)]
pub fn build_xml_block<'a>(
    result: &mut String,
    sections: &'a [String],
    matches: impl Fn(&str) -> bool,
    content_for: impl Fn(&'a str) -> Option<&'a str>,
    header: &str,
    outer_tag: &str,
    item_tag: &str,
) {
    let items: Vec<&str> = sections
        .iter()
        .map(String::as_str)
        .filter(|id| matches(id))
        .filter_map(content_for)
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .collect();
    append_items(result, &items, header, outer_tag, item_tag);
}
