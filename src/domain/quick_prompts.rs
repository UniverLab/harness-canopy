//! Ready prompts for interactive sessions (tabs + F5 menu).
//!
//! The set is the prompt builder (a special action item) followed by
//! canopy's five built-in prompts — shipped in the binary as static
//! constants, inserted as-is — followed by every `*.md` file under
//! `<canopy_dir>/prompts/quick/`, named by its file stem and sorted
//! alphabetically. A user file whose stem matches a built-in name overrides
//! that built-in's body while keeping its built-in slot; other user files
//! appear after the built-ins.

use std::path::Path;

/// Tab label of the prompt-builder action item.
pub const BUILDER_NAME: &str = "Builder";

/// Fixed one-line preview of the prompt-builder action item in the F5 menu.
pub const BUILDER_PREVIEW: &str = "Open the interactive prompt builder";

/// Built-in ready prompts, shipped in the binary and inserted as-is.
pub const ONBOARDING_BODY: &str = "You already know the user and their usual work (check your memory/context and workspace). Review how Canopy graphs work (`canopy skill get canopy-graph-design`) and propose one concrete reusable graph for a recurring process you know the user does. If you lack enough context about their work, ask them briefly with 2-3 concrete examples.";
pub const PROTOCOL_BODY: &str = "At the start of this task, read the Canopy skill (`canopy skill`) and follow Canopy's workspace protocol: inspect existing context, keep changes focused, and verify before reporting.";
pub const TUTOR_BODY: &str = "Switch into tutor mode (`canopy skill get tutor`): align on my current understanding with a few targeted questions first, then teach step by step through questions rather than lecturing.";
pub const CONTEXTUALIZATION_BODY: &str = "Before making changes, gather full context for this workspace: inspect recent git history, project structure, and Canopy project intelligence (`intelligence_get_context`), then summarize what matters for our next step.";
pub const SCHEDULE_PROMPT_BODY: &str = "Schedule a follow-up check or reminder for this session using Canopy's `scheduled_prompts` tool (`action: \"create\"`). Ask me for the delay/time and message if I haven't given them yet.";

fn builtin_slots() -> Vec<(&'static str, &'static str)> {
    vec![
        ("onboarding", ONBOARDING_BODY),
        ("protocol", PROTOCOL_BODY),
        ("tutor", TUTOR_BODY),
        ("contextualization", CONTEXTUALIZATION_BODY),
        ("schedule-prompt", SCHEDULE_PROMPT_BODY),
    ]
}

/// One entry of the ready-prompt set: the prompt builder (which opens the
/// builder dialog instead of inserting text) or a named prompt body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickItem {
    Builder,
    Prompt { name: String, body: String },
}

impl QuickItem {
    /// Tab/menu label (`Builder` for the action item, the stem for prompts).
    pub fn name(&self) -> &str {
        match self {
            QuickItem::Builder => BUILDER_NAME,
            QuickItem::Prompt { name, .. } => name,
        }
    }

    /// Insertable body, `None` for the builder action item.
    pub fn body(&self) -> Option<&str> {
        match self {
            QuickItem::Builder => None,
            QuickItem::Prompt { body, .. } => Some(body),
        }
    }

    /// One-line menu preview: the fixed builder string, or the prompt's
    /// first non-empty trimmed line truncated with `…` to `max_width`.
    pub fn preview(&self, max_width: usize) -> String {
        match self {
            QuickItem::Builder => truncate_preview(BUILDER_PREVIEW, max_width),
            QuickItem::Prompt { body, .. } => {
                let first = body
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("");
                truncate_preview(first, max_width)
            }
        }
    }
}

fn truncate_preview(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return String::new();
    }
    let kept: String = text.chars().take(max_width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Load the full set: the builder, the five built-ins (a
/// `quick/<name>.md` file overrides the matching built-in's body in place),
/// then every other `quick/*.md` file sorted by stem.
pub fn load(canopy_dir: &Path) -> Vec<QuickItem> {
    let dir = crate::domain::prompts::quick_dir(canopy_dir);
    let overrides = read_quick_dir(&dir);
    let mut items = vec![QuickItem::Builder];
    for (name, body) in builtin_slots() {
        let body = overrides
            .get(name)
            .cloned()
            .unwrap_or_else(|| body.to_string());
        items.push(QuickItem::Prompt {
            name: name.to_string(),
            body,
        });
    }
    let mut extra: Vec<(String, String)> = overrides
        .into_iter()
        .filter(|(name, _)| !builtin_slots().iter().any(|(slot, _)| slot == name))
        .collect();
    extra.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, body) in extra {
        items.push(QuickItem::Prompt { name, body });
    }
    items
}

fn read_quick_dir(dir: &Path) -> std::collections::HashMap<String, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return std::collections::HashMap::new();
    };
    let mut out = std::collections::HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if let Ok(body) = std::fs::read_to_string(&path) {
            out.insert(stem, body);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_order_is_builder_then_builtins() {
        let dir = tempfile::tempdir().unwrap();
        let items = load(dir.path());
        let names: Vec<&str> = items.iter().map(QuickItem::name).collect();
        assert_eq!(
            names,
            vec![
                "Builder",
                "onboarding",
                "protocol",
                "tutor",
                "contextualization",
                "schedule-prompt",
            ]
        );
    }

    #[test]
    fn user_file_matching_builtin_overrides_body_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let quick = crate::domain::prompts::quick_dir(dir.path());
        std::fs::create_dir_all(&quick).unwrap();
        std::fs::write(quick.join("tutor.md"), "my tutor text").unwrap();

        let items = load(dir.path());
        let names: Vec<&str> = items.iter().map(QuickItem::name).collect();
        assert_eq!(
            names,
            vec![
                "Builder",
                "onboarding",
                "protocol",
                "tutor",
                "contextualization",
                "schedule-prompt",
            ]
        );
        let tutor = items.iter().find(|item| item.name() == "tutor").unwrap();
        assert_eq!(tutor.body(), Some("my tutor text"));
    }

    #[test]
    fn other_user_files_appear_after_builtins_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let quick = crate::domain::prompts::quick_dir(dir.path());
        std::fs::create_dir_all(&quick).unwrap();
        std::fs::write(quick.join("zebra.md"), "z").unwrap();
        std::fs::write(quick.join("apple.md"), "a").unwrap();

        let items = load(dir.path());
        let names: Vec<&str> = items.iter().map(QuickItem::name).collect();
        assert_eq!(
            names,
            vec![
                "Builder",
                "onboarding",
                "protocol",
                "tutor",
                "contextualization",
                "schedule-prompt",
                "apple",
                "zebra",
            ]
        );
    }

    #[test]
    fn builder_preview_is_fixed_and_prompt_preview_is_first_line() {
        assert_eq!(
            QuickItem::Builder.preview(80),
            "Open the interactive prompt builder"
        );
        let item = QuickItem::Prompt {
            name: "x".to_string(),
            body: "\n  hello world  \nsecond line".to_string(),
        };
        assert_eq!(item.preview(80), "hello world");
    }

    #[test]
    fn preview_truncates_with_ellipsis() {
        let item = QuickItem::Prompt {
            name: "x".to_string(),
            body: "abcdefghij".to_string(),
        };
        assert_eq!(item.preview(5), "abcd…");
        assert_eq!(item.preview(10), "abcdefghij");
    }
}
