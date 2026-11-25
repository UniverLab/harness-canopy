//! Context Transfer — capture and inject a bounded summary between agents.
//!
//! Builds a plain-text context block from three toggleable parts (header,
//! visible screen tail, and last user prompt) capped at 12,000 characters,
//! then drives the two-step TUI modal (preview → agent picker).

use super::agent::InteractiveAgent;
use crate::tui::app::types::Focus;

const MAX_SCREEN_LINES: usize = 120;
const MAX_PROMPT_CHARS: usize = 1_500;
const MAX_PAYLOAD_CHARS: usize = 12_000;

// ── Parts & Payload Builder ──────────────────────────────────────

/// Toggleable parts included in a context transfer payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextTransferParts {
    pub include_header: bool,
    pub include_screen: bool,
    pub include_last_prompt: bool,
}

impl Default for ContextTransferParts {
    fn default() -> Self {
        Self {
            include_header: true,
            include_screen: true,
            include_last_prompt: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextSourceKind {
    Interactive,
    Terminal,
}

impl ContextSourceKind {
    fn header(self, agent: &InteractiveAgent) -> String {
        match self {
            Self::Interactive => format!(
                "--- context from: {} ({}) | workdir: {} ---",
                agent.name,
                agent.cli.as_str(),
                agent.working_dir
            ),
            Self::Terminal => format!(
                "--- context from terminal: {} ({}) | workdir: {} ---",
                agent.name,
                agent.cli.as_str(),
                agent.working_dir
            ),
        }
    }
}

/// Build the context transfer payload for `agent` from the enabled `parts`.
pub fn build_context_payload_for(
    agent: &InteractiveAgent,
    source_kind: ContextSourceKind,
    parts: ContextTransferParts,
) -> String {
    let header = parts.include_header.then(|| source_kind.header(agent));
    let mut screen_lines = if parts.include_screen {
        collect_visible_screen_tail(agent)
    } else {
        Vec::new()
    };
    let prompt_block = if parts.include_last_prompt {
        collect_last_prompt_block(agent)
    } else {
        None
    };

    let mut payload = assemble_payload(
        None,
        header.as_deref(),
        &screen_lines,
        prompt_block.as_deref(),
    );

    while payload.chars().count() > MAX_PAYLOAD_CHARS && !screen_lines.is_empty() {
        screen_lines.remove(0);
        let notice = format!("[context cut: kept the last {} lines]", screen_lines.len());
        payload = assemble_payload(
            Some(&notice),
            header.as_deref(),
            &screen_lines,
            prompt_block.as_deref(),
        );
    }

    if payload.chars().count() > MAX_PAYLOAD_CHARS {
        payload = payload.chars().take(MAX_PAYLOAD_CHARS).collect();
    }

    payload
}

fn collect_visible_screen_tail(agent: &InteractiveAgent) -> Vec<String> {
    let mut lines: Vec<String> = agent.visible_text().lines().map(str::to_string).collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.len() > MAX_SCREEN_LINES {
        let drop_count = lines.len() - MAX_SCREEN_LINES;
        lines.drain(..drop_count);
    }
    lines
}

fn collect_last_prompt_block(agent: &InteractiveAgent) -> Option<String> {
    let input = agent
        .prompt_history
        .lock()
        .ok()
        .and_then(|history| history.back().map(|entry| entry.input.clone()))?;
    let truncated: String = input.chars().take(MAX_PROMPT_CHARS).collect();
    let lines: Vec<String> = truncated.lines().map(|line| format!("> {line}")).collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

fn assemble_payload(
    notice: Option<&str>,
    header: Option<&str>,
    screen_lines: &[String],
    prompt_block: Option<&str>,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    if let Some(notice) = notice {
        sections.push(notice.to_string());
    }
    if let Some(header) = header {
        sections.push(header.to_string());
    }
    if !screen_lines.is_empty() {
        sections.push(screen_lines.join("\n"));
    }
    if let Some(prompt) = prompt_block {
        sections.push(prompt.to_string());
    }
    if sections.is_empty() {
        return String::new();
    }
    let mut out = sections.join("\n");
    out.push('\n');
    out
}

// ── Modal state ──────────────────────────────────────────────────

/// Which step the two-step modal is on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContextTransferStep {
    /// Step 1 — toggle payload parts and preview the payload.
    Preview,
    /// Step 2 — pick the destination agent.
    AgentPicker,
}

/// State for the context transfer modal.
pub struct ContextTransferModal {
    pub step: ContextTransferStep,
    /// Index into `App::interactive_agents` (or `terminal_agents` when `source_is_terminal`).
    pub source_agent_idx: usize,
    /// Whether the source is a terminal session (indexes `terminal_agents`).
    pub source_is_terminal: bool,
    /// Which summary parts are included in the payload.
    pub parts: ContextTransferParts,
    /// Highlighted part row in Step 1 (`0` = Header, `1` = Screen tail, `2` = Last prompt).
    pub selected_part: usize,
    /// Currently highlighted agent in the picker (index into the picker list).
    pub picker_selected: usize,
    /// Precomputed payload shown as preview in Step 1.
    pub payload_preview: String,
    /// Focus before the modal opened; the panel behind keeps drawing it.
    pub prev_focus: Option<Focus>,
}

impl ContextTransferModal {
    pub fn new(source_agent_idx: usize) -> Self {
        Self {
            step: ContextTransferStep::Preview,
            source_agent_idx,
            source_is_terminal: false,
            parts: ContextTransferParts::default(),
            selected_part: 0,
            picker_selected: 0,
            payload_preview: String::new(),
            prev_focus: None,
        }
    }

    pub fn new_terminal(source_agent_idx: usize) -> Self {
        Self {
            source_is_terminal: true,
            ..Self::new(source_agent_idx)
        }
    }

    pub fn source_kind(&self) -> ContextSourceKind {
        if self.source_is_terminal {
            ContextSourceKind::Terminal
        } else {
            ContextSourceKind::Interactive
        }
    }

    /// Toggle the currently selected part in Step 1.
    pub fn toggle_selected_part(&mut self) {
        match self.selected_part {
            0 => self.parts.include_header = !self.parts.include_header,
            1 => self.parts.include_screen = !self.parts.include_screen,
            2 => self.parts.include_last_prompt = !self.parts.include_last_prompt,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_context_payload_for, ContextSourceKind, ContextTransferModal, ContextTransferParts,
    };
    use crate::tui::agent::InteractiveAgent;
    use ratatui::style::Color;

    fn spawn_test_terminal(name: &str, rows: u16) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal("cat", "/tmp", 80, rows, Some(name), &[], Color::White)
            .expect("spawn terminal")
    }

    #[test]
    fn modal_source_kind_reflects_session_type() {
        let interactive = ContextTransferModal::new(1);
        let terminal = ContextTransferModal::new_terminal(2);
        assert_eq!(interactive.source_kind(), ContextSourceKind::Interactive);
        assert_eq!(terminal.source_kind(), ContextSourceKind::Terminal);
        assert_eq!(interactive.parts, ContextTransferParts::default());
        assert_eq!(interactive.selected_part, 0);
    }

    #[test]
    fn payload_uses_visible_screen_only_and_excludes_scrollback() {
        let agent = spawn_test_terminal("vis-only", 10);
        {
            let mut vt = agent.vt.lock().unwrap();
            for i in 0..40 {
                vt.process(format!("scrollback-or-visible line {i}\r\n").as_bytes());
            }
        }
        agent.record_prompt("check the latest output");

        let payload = build_context_payload_for(
            &agent,
            ContextSourceKind::Interactive,
            ContextTransferParts::default(),
        );

        assert!(payload.starts_with("--- context from: vis-only (cat) | workdir: /tmp ---\n"));
        assert!(!payload.contains("line 0\n"));
        assert!(payload.contains("line 35"));
        assert!(payload.contains("> check the latest output\n"));
    }

    #[test]
    fn payload_without_prompt_omits_prompt_section() {
        let agent = spawn_test_terminal("no-prompt", 10);
        {
            let mut vt = agent.vt.lock().unwrap();
            vt.process(b"hello screen\r\n");
        }

        let payload = build_context_payload_for(
            &agent,
            ContextSourceKind::Terminal,
            ContextTransferParts::default(),
        );

        assert_eq!(
            payload,
            "--- context from terminal: no-prompt (cat) | workdir: /tmp ---\nhello screen\n"
        );
    }

    #[test]
    fn payload_over_12k_drops_top_screen_lines_and_prepends_notice() {
        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            200,
            100,
            Some("cap-test"),
            &[],
            Color::White,
        )
        .expect("spawn terminal");
        {
            let mut vt = agent.vt.lock().unwrap();
            for i in 0..95 {
                let padding = "x".repeat(160);
                vt.process(format!("line-{i:03}-{padding}\r\n").as_bytes());
            }
        }

        let payload = build_context_payload_for(
            &agent,
            ContextSourceKind::Interactive,
            ContextTransferParts::default(),
        );

        assert!(payload.chars().count() <= 12_000);
        let first_line = payload.lines().next().expect("first line");
        assert!(
            first_line.starts_with("[context cut: kept the last ")
                && first_line.ends_with(" lines]"),
            "unexpected first line: {first_line}"
        );
        assert!(!payload.contains("line-000-"));
        assert!(payload.contains("line-094-"));
    }

    #[test]
    fn payload_multibyte_utf8_never_splits_mid_character() {
        let agent = spawn_test_terminal("utf8-test", 10);
        let emoji_prompt = "🦀".repeat(2_000);
        agent.record_prompt(&emoji_prompt);

        let parts = ContextTransferParts {
            include_header: false,
            include_screen: false,
            include_last_prompt: true,
        };
        let payload = build_context_payload_for(&agent, ContextSourceKind::Interactive, parts);
        // "> " (2 chars) + 1500 crabs + "\n" (1 char) = 1503 chars, valid UTF-8.
        assert_eq!(payload.chars().count(), 1503);
        assert!(payload.starts_with("> 🦀"));

        // Also test the 12,000-char fallback truncation with multibyte screen text
        // when screen is disabled? No, screen enabled with 1 long multibyte line:
        let wide_agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            15_000,
            5,
            Some("utf8-wide"),
            &[],
            Color::White,
        )
        .expect("spawn wide terminal");
        {
            let mut vt = wide_agent.vt.lock().unwrap();
            let line = "é".repeat(13_000);
            vt.process(format!("{line}\r\n").as_bytes());
        }
        let wide_payload = build_context_payload_for(
            &wide_agent,
            ContextSourceKind::Interactive,
            ContextTransferParts {
                include_header: true,
                include_screen: true,
                include_last_prompt: false,
            },
        );
        assert!(wide_payload.chars().count() <= 12_000);

        // Exercise the 12,000-char fallback truncation when screen is disabled
        // and the header itself exceeds 12,000 multibyte characters.
        let long_name = "é".repeat(12_500);
        let long_header_agent = spawn_test_terminal(&long_name, 5);
        let fallback_payload = build_context_payload_for(
            &long_header_agent,
            ContextSourceKind::Interactive,
            ContextTransferParts {
                include_header: true,
                include_screen: false,
                include_last_prompt: false,
            },
        );
        assert_eq!(fallback_payload.chars().count(), 12_000);
        assert!(fallback_payload.ends_with('é'));
    }
}
