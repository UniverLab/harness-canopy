use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::content::{
    attachment_chips_height, draw_attachment_chips, style_collapsed_paste_blocks,
    wrap_styled_content,
};
use super::geometry::{generate_bottom_border, generate_top_border};
use super::NormalContentCtx;
use crate::tui::app::dialog::SimplePromptDialog;

/// Render the ▲/▼ scroll indicator, right-aligned on row `y`.
pub(super) fn draw_scroll_arrow(
    frame: &mut Frame,
    inner: Rect,
    y: u16,
    arrow: &'static str,
    accent: Color,
) {
    let arrow = Span::styled(arrow, Style::default().fg(accent));
    let a = Rect {
        x: inner.x,
        y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(arrow)).alignment(ratatui::layout::Alignment::Right),
        a,
    );
}

/// Work backwards from `focused_section` to find the first section that fits.
pub(super) fn normal_start_idx(
    dialog: &SimplePromptDialog,
    section_heights: &[u16],
    sections_available_h: u16,
) -> usize {
    // focused_section 0 = send_at (handled above), sections start at index 1.
    let section_focus_offset = 1;
    let focused = dialog.focused_section.saturating_sub(section_focus_offset);
    let focused = focused.min(dialog.enabled_sections.len().saturating_sub(1));
    let focused_h = section_heights.get(focused).copied().unwrap_or(4);
    let mut remaining = sections_available_h.saturating_sub(focused_h);
    let mut start = focused;
    while start > 0 {
        let prev_h = section_heights.get(start - 1).copied().unwrap_or(4);
        if prev_h > remaining {
            break;
        }
        remaining -= prev_h;
        start -= 1;
    }
    start
}

/// The last section index still visible above `inner_bottom`.
pub(super) fn last_visible_section(
    dialog: &SimplePromptDialog,
    section_heights: &[u16],
    start_idx: usize,
    sections_top: u16,
    inner_bottom: u16,
) -> usize {
    let mut last = start_idx;
    let mut yy = sections_top;
    for (i, _) in dialog.enabled_sections.iter().enumerate() {
        if i < start_idx {
            continue;
        }
        let sh = section_heights.get(i).copied().unwrap_or(4);
        if yy + sh >= inner_bottom {
            break;
        }
        yy += sh;
        last = i;
    }
    last
}

/// The label to draw for a section, plus whether it is the read-only Tools one.
pub(super) fn section_display_label(
    dialog: &SimplePromptDialog,
    section_name: &str,
) -> (String, bool) {
    let section_type = {
        let known = [
            "tools",
            "instruction",
            "context",
            "project_context",
            "resources",
            "rag",
            "constraints",
            "canopy_protocol",
        ];
        known
            .iter()
            .find(|k| section_name.starts_with(*k))
            .copied()
            .unwrap_or(section_name)
    };

    let label = SimplePromptDialog::get_available_sections()
        .into_iter()
        .find(|(name, _)| *name == section_type)
        .map(|(_, label)| label)
        .unwrap_or(section_type);

    let suffix = section_name.strip_prefix(section_type).unwrap_or("");
    let is_tools = section_type == "tools";
    let display_label = if is_tools && suffix.is_empty() {
        "Tools".to_string()
    } else if is_tools {
        format!("Tools {}", suffix.trim_start_matches('_'))
    } else if suffix.is_empty() {
        label.to_string()
    } else {
        format!("{} {}", label, suffix.trim_start_matches('_'))
    };

    let is_locked = dialog.is_locked(section_name);
    let display_label = if is_locked {
        format!("{display_label} [locked]")
    } else {
        display_label
    };
    (display_label, is_tools)
}

/// Resolve the text, cursor index, content height and scroll offset to render
/// for a section, given whether it is the Tools section or currently focused.
pub(super) fn section_render_text(
    dialog: &SimplePromptDialog,
    section_name: &str,
    is_tools: bool,
    is_focused: bool,
    field_width: usize,
    inner_bottom: u16,
    y_pos: u16,
) -> (String, Option<usize>, u16, u16) {
    let content_raw = dialog
        .sections
        .get(section_name)
        .map(|s| s.as_str())
        .unwrap_or("");
    let content_real = dialog
        .collapsed_pastes
        .get(section_name)
        .map(|s| s.as_str())
        .unwrap_or(content_raw);

    if is_tools {
        // Tools section: read-only, always 1 line — shows skill label or placeholder
        let display = if content_raw.trim().is_empty() {
            "  (empty — Ctrl+A to pick a skill)".to_string()
        } else {
            content_raw.trim().to_string()
        };
        return (display, None, 1u16, 0u16);
    }
    if is_focused {
        let cursor_idx = dialog
            .cursor(section_name)
            .min(content_real.chars().count());
        let max_h = SimplePromptDialog::max_visible_lines(section_name);
        let vis = SimplePromptDialog::visual_line_count(content_real, field_width);
        // Clamp content height to available space
        let max_avail = inner_bottom.saturating_sub(y_pos).saturating_sub(2);
        return (
            content_real.to_string(),
            Some(cursor_idx),
            (vis as u16).clamp(1, max_h as u16).min(max_avail),
            dialog.scroll(section_name) as u16,
        );
    }
    let first_line = content_raw.lines().next().unwrap_or(content_raw);
    let text = if first_line.chars().count() > field_width {
        format!(
            "{}…",
            first_line
                .chars()
                .take(field_width.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        first_line.to_string()
    };
    (text, None, 1u16, 0u16)
}

/// Draw one section (label, content block, bottom border).
/// Returns the next free `y_pos` and the picker anchor when focused.
pub(super) fn draw_normal_section(
    frame: &mut Frame,
    ctx: &NormalContentCtx,
    section_name: &str,
    i: usize,
    inner_bottom: u16,
    y_pos: u16,
) -> (u16, Option<Rect>) {
    let section_focus_offset = 1; // send_at occupies focus index 0
    let is_focused = ctx.dialog.focused_section == i + section_focus_offset;
    let (display_label, is_tools) = section_display_label(ctx.dialog, section_name);

    let label_style = if is_focused {
        Style::default().fg(ctx.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.accent)
    };

    let label_line = generate_top_border(&display_label, ctx.inner.width, label_style);
    let label_area = Rect {
        x: ctx.inner.x,
        y: y_pos,
        width: ctx.inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(label_line), label_area);
    let mut y_pos = y_pos + 1;

    let section_bg = if is_focused {
        ctx.theme.selected_bg
    } else {
        ctx.theme.stage_bg
    };

    let (render_text, cursor_idx_opt, content_height, scroll_offset) = section_render_text(
        ctx.dialog,
        section_name,
        is_tools,
        is_focused,
        ctx.field_width,
        inner_bottom,
        y_pos,
    );

    let styled_content = if ctx.dialog.has_collapsed_paste(section_name) {
        // Use special styling for collapsed pastes to highlight with accent color
        style_collapsed_paste_blocks(&render_text, ctx.accent, section_bg)
    } else {
        // Use default file reference styling
        ctx.dialog
            .get_file_reference_with_styling(&render_text, ctx.accent)
    };
    let wrapped_lines = wrap_styled_content(
        styled_content,
        cursor_idx_opt,
        ctx.field_width,
        section_bg,
        ctx.theme.text_primary,
    );
    let content_paragraph =
        Paragraph::new(ratatui::text::Text::from(wrapped_lines)).scroll((scroll_offset, 0));

    let content_area = Rect {
        x: ctx.inner.x + 1,
        y: y_pos,
        width: ctx.inner.width.saturating_sub(2),
        height: content_height,
    };
    let anchor = is_focused.then_some(content_area);
    frame.render_widget(content_paragraph, content_area);
    y_pos += content_height;

    let bottom_border = generate_bottom_border(ctx.inner.width, label_style);
    let border_area = Rect {
        x: ctx.inner.x,
        y: y_pos,
        width: ctx.inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(bottom_border), border_area);
    (y_pos + 2, anchor)
}

/// Draw the scrolling section stack of the Normal tab, returning the anchor
/// area the @ file picker dropdown should hang off.
pub(super) fn draw_prompt_normal_content(
    frame: &mut Frame,
    ctx: &NormalContentCtx,
) -> Option<Rect> {
    // sections_available_h = inner height minus tab(1) + hint(1) + gap(1) at the
    // top and gap(1) + send line(1) at the bottom, and the scheduled-list
    // panel (B33) plus the attachment chips block reserved just above the
    // send line.
    let chips_h = attachment_chips_height(ctx.dialog);
    let sections_top = ctx.inner.y + 3;
    let sections_available_h = ctx
        .inner
        .height
        .saturating_sub(5 + ctx.list_panel_height + chips_h);

    let start_idx = normal_start_idx(ctx.dialog, ctx.section_heights, sections_available_h);

    // Scroll indicators. The effective bottom excludes the scheduled-list
    // panel (B33) and the chips block so sections never draw over them.
    let inner_bottom = ctx.inner.y.saturating_add(
        ctx.inner
            .height
            .saturating_sub(ctx.list_panel_height + chips_h),
    );
    if start_idx > 0 {
        draw_scroll_arrow(frame, ctx.inner, sections_top, " ▲ ", ctx.accent);
    }

    let mut y_pos = sections_top;
    let mut picker_anchor = None;

    // ── Draw all sections uniformly ─────────────────────────────────────────
    for (i, section_name) in ctx.dialog.enabled_sections.iter().enumerate() {
        // Skip sections before start_idx
        if i < start_idx {
            continue;
        }
        // Stop if we've run out of vertical space (leave 1 row for ▼ indicator)
        if y_pos + 3 >= inner_bottom {
            break;
        }
        let (next_y, anchor) =
            draw_normal_section(frame, ctx, section_name, i, inner_bottom, y_pos);
        y_pos = next_y;
        if anchor.is_some() {
            picker_anchor = anchor;
        }
    }

    // ▼ indicator when there are more sections below
    let last_visible = last_visible_section(
        ctx.dialog,
        ctx.section_heights,
        start_idx,
        sections_top,
        inner_bottom,
    );
    if last_visible < ctx.dialog.enabled_sections.len().saturating_sub(1) {
        draw_scroll_arrow(
            frame,
            ctx.inner,
            inner_bottom.saturating_sub(1),
            " ▼ ",
            ctx.accent,
        );
    }

    // Attachment chips (+ inline error) dock under the sections, above the
    // scheduled-list panel and the send line.
    if chips_h > 0 {
        draw_attachment_chips(
            frame,
            ctx.dialog,
            Rect {
                x: ctx.inner.x,
                y: inner_bottom,
                width: ctx.inner.width,
                height: chips_h,
            },
            ctx.accent,
            ctx.theme,
        );
    }
    picker_anchor
}
