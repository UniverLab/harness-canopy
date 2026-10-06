use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    cron_value, filter_row, focus_style, identity_label, picker_item_style,
    prompt_visual_line_count, push_spaced_row, selector_spans, truncate_str, FieldLayout,
    PickerWindow, PROMPT_VISIBLE_ROWS, TERMINAL_SHELL_FIELD,
};
use crate::tui::app::dialog::new_agent::{BackgroundTrigger, NewAgentDialog};
use crate::tui::ui::theme::Theme;

pub(super) fn append_prompt_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    layout: FieldLayout,
    field_width: usize,
    theme: &Theme,
) {
    push_spaced_row(
        lines,
        background_prompt_label(dialog, accent, layout.prompt, theme),
    );
    append_prompt_input_block(lines, dialog, accent, layout.prompt, field_width, theme);
    push_spaced_row(
        lines,
        background_target_row(dialog, accent, layout.extra, theme),
    );
}

fn background_prompt_label(
    dialog: &NewAgentDialog,
    accent: Color,
    prompt_field: usize,
    theme: &Theme,
) -> Line<'static> {
    let focus = focus_style(dialog.field, prompt_field, accent);
    Line::from(vec![
        Span::styled("  Prompt: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            format!(" {} rows ", PROMPT_VISIBLE_ROWS),
            focus.add_modifier(Modifier::DIM),
        ),
    ])
}

/// Wrap the prompt char-accurately at `field_width` and render `PROMPT_VISIBLE_ROWS`
/// lines starting from `dialog.prompt_scroll`, drawing a block cursor at
/// `dialog.prompt_cursor`. Mirrors the math in `prompt_visual_line_count` so
/// scrolling, the input handler, and the render stay in lockstep.
#[allow(clippy::too_many_arguments)]
fn append_prompt_input_block(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    prompt_field: usize,
    field_width: usize,
    theme: &Theme,
) {
    let field_width = field_width.max(1);
    let indent = "  ";
    let is_focused = dialog.field == prompt_field;
    let placeholder = "enter agent prompt…";
    let cursor_style = if is_focused {
        Style::default()
            .fg(Color::Black)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.dim_text)
    };
    let base_style = if is_focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(theme.dim_text)
    };

    for row in 0..PROMPT_VISIBLE_ROWS {
        let line_index = dialog.prompt_scroll + row;
        let mut spans: Vec<Span<'static>> =
            vec![Span::styled(indent, Style::default().fg(theme.dim_text))];
        if dialog.prompt.is_empty() {
            // Empty prompt: show placeholder only on the first row, otherwise
            // leave a blank cell so the cursor still has a visible home.
            if line_index == 0 {
                if is_focused && dialog.prompt_cursor == 0 {
                    spans.push(Span::styled(" ", cursor_style));
                } else {
                    spans.push(Span::styled(
                        placeholder.chars().take(field_width).collect::<String>(),
                        Style::default()
                            .fg(theme.dim_text)
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            } else if is_focused {
                spans.push(Span::styled(" ", cursor_style));
            }
            lines.push(Line::from(spans));
            continue;
        }

        let char_range = prompt_visual_line_range(dialog, line_index, field_width);
        let chars_in_row: String = dialog
            .prompt
            .chars()
            .skip(char_range.0)
            .take(char_range.1 - char_range.0)
            .collect();
        let mut col = 0usize;
        let mut char_pos = char_range.0;
        for ch in chars_in_row.chars() {
            if ch == '\n' {
                char_pos += 1;
                continue;
            }
            if col >= field_width {
                break;
            }
            let style = if is_focused && char_pos == dialog.prompt_cursor {
                cursor_style
            } else {
                base_style
            };
            spans.push(Span::styled(ch.to_string(), style));
            col += 1;
            char_pos += 1;
        }
        // Cursor at the end of the last row when it sits on a non-existent
        // cell — draw a highlighted blank so the caret stays visible.
        if is_focused
            && dialog.prompt_cursor == char_pos
            && line_index + 1 >= prompt_visual_line_count(dialog, field_width)
        {
            spans.push(Span::styled(" ", cursor_style));
        }
        lines.push(Line::from(spans));
    }
}

/// Char index range `[start, end)` that maps to `visual_line_index` when the
/// prompt is wrapped at `field_width`. Hard newlines count as line breaks and
/// are skipped by the renderer (which flushes the current line at `\n`).
pub(super) fn prompt_visual_line_range(
    dialog: &NewAgentDialog,
    visual_line_index: usize,
    field_width: usize,
) -> (usize, usize) {
    let field_width = field_width.max(1);
    let mut line_start = 0usize;
    let mut line_index = 0usize;
    let mut col = 0usize;
    let iter = dialog.prompt.char_indices();
    for (_, ch) in iter {
        if ch == '\n' {
            if line_index == visual_line_index {
                return (line_start, line_start + col);
            }
            line_index += 1;
            line_start += col + 1; // skip the '\n' itself
            col = 0;
            continue;
        }
        if col >= field_width {
            if line_index == visual_line_index {
                return (line_start, line_start + field_width);
            }
            line_index += 1;
            line_start += field_width;
            col = 0;
        }
        col += 1;
    }
    if line_index == visual_line_index {
        return (line_start, line_start + col);
    }
    (dialog.prompt.chars().count(), dialog.prompt.chars().count())
}

fn background_target_row(
    dialog: &NewAgentDialog,
    accent: Color,
    extra_field: usize,
    theme: &Theme,
) -> Line<'static> {
    match dialog.background_trigger {
        BackgroundTrigger::Cron => Line::from(vec![
            Span::styled("  Cron:  ", Style::default().fg(theme.dim_text)),
            Span::styled(
                cron_value(dialog),
                focus_style(dialog.field, extra_field, accent),
            ),
        ]),
        BackgroundTrigger::Watch => Line::from(vec![
            Span::styled("  Path:  ", Style::default().fg(theme.dim_text)),
            Span::styled(
                truncate_str(&dialog.watch_path, 50),
                focus_style(dialog.field, extra_field, accent),
            ),
        ]),
    }
}

pub(super) fn append_identity_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    identity_field: usize,
    theme: &Theme,
) {
    let label = identity_label(dialog);
    let locked = dialog.seed_options.len() <= 1;
    let mut identity_row = vec![Span::styled(
        "  Identity: ",
        Style::default().fg(theme.dim_text),
    )];
    identity_row.extend(selector_spans(
        &label,
        identity_field,
        locked,
        accent,
        dialog.field,
        theme,
    ));
    push_spaced_row(lines, Line::from(identity_row));
}

pub(super) fn append_yolo_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    yolo_field: usize,
    theme: &Theme,
) {
    let has_yolo = dialog.selected_yolo_flag().is_some();
    let checkbox = if dialog.yolo_mode { "◉" } else { "○" };
    let checkbox_style = if dialog.field == yolo_field {
        focus_style(dialog.field, yolo_field, accent)
    } else if dialog.yolo_mode {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let mut spans = vec![
        Span::styled("  Yolo:  ", Style::default().fg(theme.dim_text)),
        Span::styled(format!("{checkbox} Autonomous mode"), checkbox_style),
    ];
    if !has_yolo {
        spans.push(Span::styled(
            "  (not supported by this harness)",
            Style::default().fg(theme.dim_text),
        ));
    } else if dialog.yolo_mode {
        spans.push(Span::styled(
            "  ⚠ agent acts without approval",
            Style::default().fg(Color::Yellow),
        ));
    }

    push_spaced_row(lines, Line::from(spans));
}

pub(super) fn append_sandbox_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    sandbox_field: usize,
    theme: &Theme,
) {
    if sandbox_field == 0 {
        return;
    }
    let checkbox = if dialog.sandbox_mode { "◉" } else { "○" };
    let checkbox_style = if dialog.field == sandbox_field {
        focus_style(dialog.field, sandbox_field, accent)
    } else if dialog.sandbox_mode {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let mut spans = vec![
        Span::styled("  Sandbox:  ", Style::default().fg(theme.dim_text)),
        Span::styled(format!("{checkbox} Isolated worktree"), checkbox_style),
    ];
    if dialog.sandbox_mode {
        spans.push(Span::styled(
            "  no protocol file in repo",
            Style::default().fg(Color::Cyan),
        ));
    }

    push_spaced_row(lines, Line::from(spans));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_directory_section(
    lines: &mut Vec<Line<'static>>,
    dialog: &NewAgentDialog,
    accent: Color,
    dir_field: usize,
    hide_dir: bool,
    browser_field: usize,
    theme: &Theme,
    dir_visible: usize,
) {
    if !hide_dir {
        push_spaced_row(lines, working_dir_row(dialog, accent, dir_field, theme));
    }
    if dialog.dir_entries.is_empty() {
        return;
    }

    lines.extend(dir_browser_lines(
        dialog,
        accent,
        dialog.field == browser_field,
        theme,
        dir_visible,
    ));
}

fn working_dir_row(
    dialog: &NewAgentDialog,
    accent: Color,
    dir_field: usize,
    theme: &Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled("  Dir:   ", Style::default().fg(theme.dim_text)),
        Span::styled(
            truncate_str(&dialog.working_dir, 50),
            focus_style(dialog.field, dir_field, accent),
        ),
    ])
}

pub(super) fn terminal_shell_row(
    dialog: &NewAgentDialog,
    accent: Color,
    theme: &Theme,
) -> Line<'static> {
    let shell_display = if dialog.available_shells.len() > 1 {
        format!("◂ {} ▸", dialog.selected_shell())
    } else {
        dialog.selected_shell().to_string()
    };

    Line::from(vec![
        Span::styled("  Shell: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            format!(" {} ", shell_display),
            focus_style(dialog.field, TERMINAL_SHELL_FIELD, accent),
        ),
    ])
}

// ── Navigation / picker rendering ────────────────────────────────────────────

fn dir_browser_lines(
    dialog: &NewAgentDialog,
    accent: Color,
    focused: bool,
    theme: &Theme,
    max_visible: usize,
) -> Vec<Line<'static>> {
    let filtered = dialog.filtered_dir_entries();
    let window = PickerWindow::new(dialog.dir_selected, filtered.len(), max_visible);
    let mut lines = vec![filter_row(
        "  filter: ",
        &dialog.dir_filter,
        focused,
        accent,
        theme,
    )];

    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no matches)",
            Style::default().fg(theme.dim_text),
        )));
    } else {
        for (i, entry) in filtered
            .iter()
            .enumerate()
            .skip(window.scroll)
            .take(max_visible)
        {
            let style = picker_item_style(accent, i == dialog.dir_selected);
            lines.push(Line::from(Span::styled(format!("    {entry}"), style)));
        }
    }

    let footer = if filtered.is_empty() {
        "    0 items".to_string()
    } else {
        let up = if window.has_above { "↑ " } else { "  " };
        let down = if window.has_below { " ↓" } else { "  " };
        format!(
            "    {up}{}/{}{down}",
            dialog.dir_selected + 1,
            filtered.len()
        )
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme.dim_text),
    )));
    lines.push(Line::from(""));
    lines
}
