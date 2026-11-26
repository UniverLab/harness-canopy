//! Rendering for Step 1 (`FolderAndKind`) and Step 2 (`AgentOptions`) of the
//! 3-step Ctrl+N `NewAgentDialog`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::fields::{
    append_directory_section, append_sandbox_section, append_yolo_section, DirSection,
};
use super::pickers::append_cli_section;
use super::{prompt_field_width, selector_spans, visible_dir_rows};
use crate::tui::app::dialog::new_agent::{NewAgentDialog, NewTaskType};
use crate::tui::ui::theme::Theme;

/// Render Step 1: working folder and kind (`Agent` or `Terminal`).
pub(super) fn build_step1_lines(
    dialog: &NewAgentDialog,
    theme: &Theme,
    frame_area: Rect,
) -> Vec<Line<'static>> {
    let accent = theme.header_color;
    let filtered_clis = dialog.filtered_cli_indices();
    let field_width = prompt_field_width(frame_area);
    let dir_visible = visible_dir_rows(
        dialog,
        accent,
        &filtered_clis,
        field_width,
        theme,
        frame_area,
    );

    let kind_label = match dialog.task_type {
        NewTaskType::Terminal => "Terminal",
        _ => "Agent",
    };
    let action_label = if matches!(dialog.task_type, NewTaskType::Terminal) {
        "[ Launch → ]"
    } else {
        "[ Next → ]"
    };

    let mut kind_row = vec![Span::styled(
        "  Kind:  ",
        Style::default().fg(theme.dim_text),
    )];
    kind_row.extend(selector_spans(
        kind_label,
        0,
        false,
        accent,
        dialog.field,
        theme,
    ));
    let used_len: usize = kind_row.iter().map(|s| s.content.chars().count()).sum();
    let pad = 32usize.saturating_sub(used_len).max(2);
    kind_row.push(Span::raw(" ".repeat(pad)));
    kind_row.push(Span::styled(
        action_label,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));

    let mut lines = vec![Line::from(""), Line::from(kind_row), Line::from("")];
    append_directory_section(
        &mut lines,
        dialog,
        accent,
        theme,
        DirSection {
            field: 1,
            hidden: false,
            browser_field: 1,
            visible: dir_visible,
        },
    );

    let enter_hint = if matches!(dialog.task_type, NewTaskType::Terminal) {
        "Enter: launch"
    } else {
        "Enter: next"
    };
    lines.push(Line::from(Span::styled(
        format!(
            "  ↑↓: fields · ◂▸/Space: kind · (dirs: → enter ← up) · {enter_hint} · Esc: cancel"
        ),
        Style::default().fg(theme.dim_text),
    )));
    lines
}

/// Render Step 2 (agent only): harness, yolo, and sandbox on one screen.
pub(super) fn build_step2_lines(
    dialog: &NewAgentDialog,
    filtered_clis: &[usize],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let accent = theme.header_color;
    let mut lines = vec![Line::from("")];
    append_cli_section(&mut lines, dialog, accent, filtered_clis, 0, theme);
    append_yolo_section(&mut lines, dialog, accent, 1, theme);
    append_sandbox_section(&mut lines, dialog, accent, 2, theme);

    lines.push(Line::from(vec![
        Span::styled("  [ ← Back ]", Style::default().fg(theme.dim_text)),
        Span::raw("      "),
        Span::styled(
            "[ Next → ]",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓: fields · ◂▸/Space: toggle · Enter: next (Mode) · Esc: back",
        Style::default().fg(theme.dim_text),
    )));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step1_does_not_render_background_option() {
        let d = NewAgentDialog::new(Some("."));
        let lines = build_step1_lines(&d, &Theme::classic(), Rect::new(0, 0, 100, 30));
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("Background"), "{text}");
        assert!(text.contains("Agent"), "{text}");
    }
}
